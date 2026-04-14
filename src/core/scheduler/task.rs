use std::{
    alloc::Layout,
    future::Future,
    ptr::{self, NonNull},
    sync::Arc,
    sync::atomic::{AtomicBool, AtomicPtr, AtomicU8, AtomicUsize, Ordering},
    task::{Context as StdContext, RawWaker, RawWakerVTable, Waker},
};

use crossbeam::utils::CachePadded;

use crate::core::executor::context::Context;
use crate::core::scheduler::scheduler::Scheduler;

#[repr(align(64))]
pub(crate) struct TaskHeader {
    pub(crate) ref_count: CachePadded<AtomicUsize>,
    pub(crate) notified: CachePadded<AtomicBool>,
    pub(crate) result_state: CachePadded<AtomicU8>, // 0: running, 1: completed, 2: panicked, 3: joined
    pub(crate) join_waker: CachePadded<AtomicPtr<()>>,

    pub(crate) scheduler: Arc<Scheduler>,
    pub(crate) vtable: &'static TaskVTable,
    pub(crate) layout: Layout,
    pub(crate) future_offset: usize,
}

pub(crate) struct TaskVTable {
    pub(crate) poll: unsafe fn(NonNull<TaskHeader>, &mut StdContext<'_>),
    pub(crate) wake: unsafe fn(NonNull<TaskHeader>),
    pub(crate) wake_by_ref: unsafe fn(NonNull<TaskHeader>),
    pub(crate) drop_payload: unsafe fn(NonNull<TaskHeader>),
    pub(crate) read_result: unsafe fn(NonNull<TaskHeader>, *mut u8),
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
                (vtable.drop_payload)(self.ptr);

                let layout = self.ptr.as_ref().layout;
                let ptr = self.ptr.as_ptr() as *mut u8;

                Context::with(|ctx| ctx.task_pool.deallocate(ptr, layout));
            }
        }
    }
}

pub struct Task;

impl Task {
    pub(crate) fn spawn<F, T>(future: F, scheduler: Arc<Scheduler>) -> TaskRef
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let header_layout = Layout::new::<TaskHeader>();
        let future_layout = Layout::new::<F>();
        let result_layout = Layout::new::<T>();

        let size = std::cmp::max(future_layout.size(), result_layout.size());
        let align = std::cmp::max(future_layout.align(), result_layout.align());
        let payload_layout = Layout::from_size_align(size, align).unwrap();

        let (joint_layout, future_offset) = header_layout.extend(payload_layout).unwrap();
        let joint_layout = joint_layout.pad_to_align();

        let ptr = Context::with(|ctx| ctx.task_pool.allocate(joint_layout));
        let header_ptr = ptr as *mut TaskHeader;

        unsafe {
            ptr::write(
                header_ptr,
                TaskHeader {
                    ref_count: CachePadded::new(AtomicUsize::new(1)),
                    notified: CachePadded::new(AtomicBool::new(false)),
                    scheduler,
                    vtable: &TaskVTable {
                        poll: Self::poll_fn::<F, T>,
                        wake: Self::wake_fn,
                        wake_by_ref: Self::wake_by_ref_fn,
                        drop_payload: Self::drop_payload_fn::<F, T>,
                        read_result: Self::read_result_fn::<T>,
                    },
                    layout: joint_layout,
                    future_offset,
                    result_state: CachePadded::new(AtomicU8::new(0)), // Running
                    join_waker: CachePadded::new(AtomicPtr::new(ptr::null_mut())),
                },
            );

            let future_ptr = ptr.add(future_offset) as *mut F;
            ptr::write(future_ptr, future);

            TaskRef::from_raw(NonNull::new_unchecked(header_ptr))
        }
    }

    unsafe fn poll_fn<F, T>(ptr: NonNull<TaskHeader>, cx: &mut StdContext<'_>)
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let (future_ptr, waker) = unsafe {
            let header = ptr.as_ref();
            let future_ptr = (ptr.as_ptr() as *mut u8).add(header.future_offset) as *mut F;
            (future_ptr, cx.waker())
        };

        let future = unsafe { &mut *future_ptr };
        let mut cx = std::task::Context::from_waker(waker);
        unsafe {
            match std::pin::Pin::new_unchecked(future).poll(&mut cx) {
                std::task::Poll::Ready(val) => {
                    let header = ptr.as_ref();
                    // Drop future to reclaim resources, then write result
                    ptr::drop_in_place(future_ptr);

                    let result_ptr = (ptr.as_ptr() as *mut u8).add(header.future_offset) as *mut T;
                    ptr::write(result_ptr, val);

                    header.result_state.store(1, Ordering::SeqCst); // Completed

                    // Wake the joiner if present
                    let waker_ptr = header.join_waker.swap(ptr::null_mut(), Ordering::SeqCst);
                    if !waker_ptr.is_null() {
                        let waker = Box::from_raw(waker_ptr as *mut Waker);
                        waker.wake();
                    }
                }
                std::task::Poll::Pending => {}
            }
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

    unsafe fn drop_payload_fn<F, T>(ptr: NonNull<TaskHeader>) {
        unsafe {
            let header = ptr.as_ref();
            let state = header.result_state.load(Ordering::Acquire);

            if state == 0 {
                // Future is still there
                let future_ptr = (ptr.as_ptr() as *mut u8).add(header.future_offset) as *mut F;
                ptr::drop_in_place(future_ptr);
            } else if state == 1 {
                // Result is there
                let result_ptr = (ptr.as_ptr() as *mut u8).add(header.future_offset) as *mut T;
                ptr::drop_in_place(result_ptr);
            }
            // State 3 (Joined) means result was already moved out.

            ptr::drop_in_place(ptr.as_ptr());
        }
    }

    unsafe fn read_result_fn<T>(ptr: NonNull<TaskHeader>, dest: *mut u8) {
        unsafe {
            let header = ptr.as_ref();
            let result_ptr = (ptr.as_ptr() as *mut u8).add(header.future_offset) as *mut T;
            let dest_ptr = dest as *mut T;
            ptr::copy_nonoverlapping(result_ptr, dest_ptr, 1);
            header.result_state.store(3, Ordering::SeqCst); // Joined
        }
    }

    fn wake_internal(this: TaskRef) {
        if !Context::try_push_local(this.clone()) {
            let scheduler = unsafe { this.ptr.as_ref().scheduler.clone() };
            scheduler.global_queue.push(this);
            scheduler.notify_adaptive();
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
        assert!(future_offset < 1024);
    }

    #[test]
    fn test_header_physical_alignment() {
        use std::mem::align_of;

        // TaskHeader should be meaningfully aligned
        assert!(
            align_of::<TaskHeader>() >= 64,
            "TaskHeader should have 64-byte alignment"
        );

        static DUMMY_VTABLE: TaskVTable = TaskVTable {
            poll: |_, _| {},
            wake: |_| {},
            wake_by_ref: |_| {},
            drop_payload: |_| {},
            read_result: |_, _| {},
        };

        let header = TaskHeader {
            ref_count: CachePadded::new(AtomicUsize::new(1)),
            notified: CachePadded::new(AtomicBool::new(false)),
            scheduler: Arc::new(Scheduler::new()),
            vtable: &DUMMY_VTABLE,
            layout: Layout::new::<()>(),
            future_offset: 0,
            result_state: CachePadded::new(AtomicU8::new(0)),
            join_waker: CachePadded::new(AtomicPtr::new(ptr::null_mut())),
        };

        let base = &header as *const _ as usize;
        let notified_addr = &header.notified as *const _ as usize;
        let join_waker_addr = &header.join_waker as *const _ as usize;

        let notified_offset = notified_addr - base;
        let join_waker_offset = join_waker_addr - base;

        // Verify isolation
        let delta = if join_waker_offset > notified_offset {
            join_waker_offset - notified_offset
        } else {
            notified_offset - join_waker_offset
        };

        assert!(
            delta >= 64,
            "Contended fields (notified/join_waker) should be in separate cache lines (delta: {})",
            delta
        );
    }

    #[test]
    fn test_task_payload_cleanup() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        
        static DROP_COUNT: AtomicUsize = AtomicUsize::new(0);
        
        struct DropTracker;
        impl Drop for DropTracker {
            fn drop(&mut self) {
                DROP_COUNT.fetch_add(1, Ordering::SeqCst);
            }
        }
        
        let scheduler = Arc::new(Scheduler::new());
        
        // CASE 1: Task completed and joined
        {
            let tracker = DropTracker;
            let task = Task::spawn(async move {
                let _t = tracker;
                42
            }, scheduler.clone());
            
            let waker = task.waker();
            let mut cx = std::task::Context::from_waker(&waker);
            
            // Poll to completion
            unsafe {
                (task.as_ptr().as_ref().vtable.poll)(task.as_ptr(), &mut cx);
            }
            
            // Ref count is 1 (task_ref). 
            // Future should be dropped. Drop count should be 1.
            assert_eq!(DROP_COUNT.load(Ordering::SeqCst), 1);
            
            drop(task);
        }
        
        // Reset count
        DROP_COUNT.store(0, Ordering::SeqCst);
        
        // CASE 2: Task cancelled (dropped before completion)
        {
            let tracker = DropTracker;
            let task = Task::spawn(async move {
                let _t = tracker;
            }, scheduler.clone());
            
            // Currently ref_count is 1.
            drop(task);
            // ref_count becomes 0, payload should be dropped.
            assert_eq!(DROP_COUNT.load(Ordering::SeqCst), 1);
        }
    }
}

