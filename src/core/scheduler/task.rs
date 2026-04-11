use std::{
    alloc::Layout,
    ptr::{self, NonNull},
    sync::Arc,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    task::{Context as StdContext, RawWaker, RawWakerVTable, Waker},
};

use crate::core::executor::context::Context;
use crate::core::scheduler::scheduler::Scheduler;

pub(crate) struct TaskHeader {
    pub(crate) ref_count: AtomicUsize,
    pub(crate) notified: AtomicBool,
    pub(crate) scheduler: Arc<Scheduler>,
    pub(crate) vtable: &'static TaskVTable,
    pub(crate) layout: Layout,
    pub(crate) future_offset: usize,
}

pub(crate) struct TaskVTable {
    pub(crate) poll: unsafe fn(NonNull<TaskHeader>, &mut StdContext<'_>),
    pub(crate) wake: unsafe fn(NonNull<TaskHeader>),
    pub(crate) wake_by_ref: unsafe fn(NonNull<TaskHeader>),
    pub(crate) drop_task: unsafe fn(NonNull<TaskHeader>),
}

pub struct TaskRef {
    ptr: NonNull<TaskHeader>,
}

unsafe impl Send for TaskRef {}
unsafe impl Sync for TaskRef {}

impl TaskRef {
    pub(crate) unsafe fn from_raw(ptr: NonNull<TaskHeader>) -> Self {
        Self { ptr }
    }

    pub(crate) fn as_ptr(&self) -> NonNull<TaskHeader> {
        self.ptr
    }

    pub fn waker(&self) -> Waker {
        let vtable = &TASK_RAW_WAKER_VTABLE;
        let cloned = self.clone();
        let ptr = cloned.ptr.as_ptr() as *const ();
        std::mem::forget(cloned); // Ownership transferred to RawWaker
        unsafe { Waker::from_raw(RawWaker::new(ptr, vtable)) }
    }
}

impl Clone for TaskRef {
    fn clone(&self) -> Self {
        unsafe {
            self.ptr.as_ref().ref_count.fetch_add(1, Ordering::Relaxed);
        }
        Self { ptr: self.ptr }
    }
}

impl Drop for TaskRef {
    fn drop(&mut self) {
        unsafe {
            if self.ptr.as_ref().ref_count.fetch_sub(1, Ordering::Release) == 1 {
                self.ptr.as_ref().ref_count.load(Ordering::Acquire);
                let vtable = self.ptr.as_ref().vtable;
                (vtable.drop_task)(self.ptr);

                let layout = self.ptr.as_ref().layout;
                let ptr = self.ptr.as_ptr() as *mut u8;

                Context::with(|ctx| ctx.task_pool.deallocate(ptr, layout));
            }
        }
    }
}

pub struct Task;

impl Task {
    pub(crate) fn spawn<F>(future: F, scheduler: Arc<Scheduler>) -> TaskRef
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let header_layout = Layout::new::<TaskHeader>();
        let future_layout = Layout::new::<F>();
        let (joint_layout, future_offset) = header_layout.extend(future_layout).unwrap();
        let joint_layout = joint_layout.pad_to_align();

        let ptr = Context::with(|ctx| ctx.task_pool.allocate(joint_layout));
        let header_ptr = ptr as *mut TaskHeader;

        unsafe {
            ptr::write(
                header_ptr,
                TaskHeader {
                    ref_count: AtomicUsize::new(1),
                    notified: AtomicBool::new(false),
                    scheduler,
                    vtable: &TaskVTable {
                        poll: Self::poll_fn::<F>,
                        wake: Self::wake_fn,
                        wake_by_ref: Self::wake_by_ref_fn,
                        drop_task: Self::drop_task_fn::<F>,
                    },
                    layout: joint_layout,
                    future_offset,
                },
            );

            let future_ptr = ptr.add(future_offset) as *mut F;
            ptr::write(future_ptr, future);

            TaskRef::from_raw(NonNull::new_unchecked(header_ptr))
        }
    }

    unsafe fn poll_fn<F: Future<Output = ()>>(ptr: NonNull<TaskHeader>, cx: &mut StdContext<'_>) {
        let (future_ptr, waker) = unsafe {
            let header = ptr.as_ref();
            let future_ptr = (ptr.as_ptr() as *mut u8).add(header.future_offset) as *mut F;
            (future_ptr, cx.waker())
        };

        let future = unsafe { &mut *future_ptr };
        let mut cx = std::task::Context::from_waker(waker);
        unsafe {
            let _ = std::pin::Pin::new_unchecked(future).poll(&mut cx);
        }
    }

    unsafe fn wake_fn(ptr: NonNull<TaskHeader>) {
        let task_ref = unsafe { TaskRef::from_raw(ptr) };
        if unsafe { !task_ref.ptr.as_ref().notified.swap(true, Ordering::SeqCst) } {
            Self::wake_internal(task_ref);
        }
    }

    unsafe fn wake_by_ref_fn(ptr: NonNull<TaskHeader>) {
        let header = unsafe { ptr.as_ref() };
        if !header.notified.swap(true, Ordering::SeqCst) {
            let task_ref = TaskRef { ptr };
            header.ref_count.fetch_add(1, Ordering::Relaxed);
            Self::wake_internal(task_ref);
        }
    }

    unsafe fn drop_task_fn<F: Future<Output = ()>>(ptr: NonNull<TaskHeader>) {
        unsafe {
            let header = ptr.as_ref();
            let future_ptr = (ptr.as_ptr() as *mut u8).add(header.future_offset) as *mut F;
            ptr::drop_in_place(future_ptr);
            ptr::drop_in_place(ptr.as_ptr());
        }
    }

    fn wake_internal(this: TaskRef) {
        if !Context::try_push_local(this.clone()) {
            let scheduler = unsafe { this.ptr.as_ref().scheduler.clone() };
            scheduler.global_queue.push(this);
        }
    }
}

static TASK_RAW_WAKER_VTABLE: RawWakerVTable = RawWakerVTable::new(
    |ptr| {
        let task_ref = unsafe { &*(ptr as *const TaskHeader) };
        task_ref.ref_count.fetch_add(1, Ordering::Relaxed);
        RawWaker::new(ptr, &TASK_RAW_WAKER_VTABLE)
    },
    |ptr| unsafe {
        let ptr = NonNull::new_unchecked(ptr as *mut TaskHeader);
        let vtable = ptr.as_ref().vtable;
        (vtable.wake)(ptr);
    },
    |ptr| unsafe {
        let ptr = NonNull::new_unchecked(ptr as *mut TaskHeader);
        let vtable = ptr.as_ref().vtable;
        (vtable.wake_by_ref)(ptr);
    },
    |ptr| unsafe {
        let _ = TaskRef::from_raw(NonNull::new_unchecked(ptr as *mut TaskHeader));
    },
);

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn test_task_construction_and_waking() {
        let scheduler = Arc::new(Scheduler::new());
        let task = Task::spawn(async {}, scheduler.clone());

        // Initial state: nothing in global queue
        assert!(scheduler.global_queue.steal().is_none());

        // Wake the task
        task.waker().wake_by_ref();

        // Verify it was pushed to the global queue
        let stolen = scheduler
            .global_queue
            .steal()
            .expect("Task should be in global queue");
        assert_eq!(task.as_ptr(), stolen.as_ptr());
    }

    #[test]
    fn test_redundant_wake_refcount() {
        let scheduler = Arc::new(Scheduler::new());
        let task = Task::spawn(async {}, scheduler.clone());

        let initial_count = unsafe { task.as_ptr().as_ref().ref_count.load(Ordering::Relaxed) };
        assert_eq!(initial_count, 1);

        // Multiple wakes should NOT increase the refcount indefinitely.
        let waker = task.waker();
        for _ in 0..10 {
            waker.wake_by_ref();
        }

        let after_wakes_count = unsafe { task.as_ptr().as_ref().ref_count.load(Ordering::Relaxed) };
        // Expecting 3: (1 for 'task' variable, 1 for 'waker', 1 for being in queue)
        assert_eq!(
            after_wakes_count, 3,
            "Refcount should only increase once when woken multiple times"
        );

        // After stealing (obtaining another TaskRef), it should still be 3.
        let stolen = scheduler.global_queue.steal().unwrap();
        assert_eq!(
            unsafe { task.as_ptr().as_ref().ref_count.load(Ordering::Relaxed) },
            3
        );
        drop(stolen);
        assert_eq!(
            unsafe { task.as_ptr().as_ref().ref_count.load(Ordering::Relaxed) },
            2
        );
    }

    #[test]
    fn test_task_memory_locality() {
        let scheduler = Arc::new(Scheduler::new());
        let future = async {
            let _data = [0u8; 128];
        };

        let task = Task::spawn(future, scheduler);

        let header_addr = task.as_ptr().as_ptr() as usize;
        let future_offset = unsafe { task.as_ptr().as_ref().future_offset };
        let expected_future_addr = header_addr + future_offset;

        // In the new unified allocation system, the future lives EXACTLY at offset
        // from the header.
        println!(
            "Header addr: {:x}, Future offset: {}, Expected Future addr: {:x}",
            header_addr, future_offset, expected_future_addr
        );

        assert!(future_offset >= std::mem::size_of::<TaskHeader>());
        assert!(future_offset < 256);
    }
}
