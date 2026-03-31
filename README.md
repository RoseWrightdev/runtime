# Async Runtime

Implementation of a work-stealing asynchronous executor and reactor.

## Architecture

The system consists of a global scheduler, worker threads, and a single-threaded reactor.


### 1. Request Lifecycle

The following sequence documents the interaction between components during an asynchronous I/O operation (e.g., `TcpStream::read`).

```mermaid
sequenceDiagram
    participant T as Task (Future)
    participant W as Worker Thread
    participant R as Reactor Thread
    participant K as Kernel (OS)

    Note over W,T: Task Execution Level
    activate W
    W->>+T: 1. poll()
    T-->>-W: 2. return Poll::Pending
    
    Note over W,R: I/O Registration
    W->>+R: 3. register(fd, waker)
    W->>W: 4. park() or find next task
    deactivate W

    Note over R,K: Event Multiplexing (epoll/kqueue)
    R->>+K: 5. wait(timeout)
    K-->>-R: 6. Event Ready (FD)
    
    Note over R,T: Waker triggers re-scheduling
    R->>+T: 7. waker.wake()
    T->>W: 8. inject(task) into Scheduler
    deactivate T
    deactivate R
    
    Note over W,T: Task Completion
    activate W
    W->>+T: 9. poll() again
    T-->>-W: 10. return Poll::Ready(n)
    deactivate W
```

### 2. Task Scheduling

Tasks are distributed via a multi-level queue hierarchy:
- **LIFO Slot**: Thread-local storage for the most recently woken task.
- **Local Deque**: Per-worker FIFO/LIFO queue.
- **Global Queue**: Shared injector for external tasks.

```mermaid
graph LR
    subgraph "Worker"
        L[LIFO Slot]
        D[Local Deque]
    end
    subgraph "Global"
        G[Global Injector]
    end

    L -- Push/Pop --- D
    G -- Pull --- D
```

### 3. State Management

Task coordination is handled via an `AtomicU8` state machine.

```mermaid
stateDiagram-v2
    [*] --> SCHEDULED
    SCHEDULED --> POLLING
    POLLING --> IDLE: Pending
    POLLING --> COMPLETED: Ready
    IDLE --> SCHEDULED: Reactor Waker
```

## Performance Data

Measured using `cargo run --release` with 1024-byte payloads (MacOS).

| Concurrency | Total Messages | Runtime MiB/s | vs. Tokio MiB/s |
| :--- | :--- | :--- | :--- |
| **100** | 100,000 | 131.25 | 138.16 |
| **1,000** | 1,000,000 | 130.43 | 159.06 |
| **5,000** | 10,000,000 | 128.21 | 374.80 |

### Observed Scaling Bottlenecks
Beyond 1,000 concurrent connections, the throughput of the custom runtime remains constant while baseline Tokio continues to scale. This is attributed to:
- Serialized FD registration via a single `SegQueue`.
- Syscall overhead of the Reactor's notification mechanism.

## Request Lifecycle

The following sequence documents the interaction between components during an asynchronous I/O operation (e.g., `TcpStream::read`).

```mermaid
sequenceDiagram
    participant T as Task (Future)
    participant W as Worker Thread
    participant R as Reactor Thread
    participant K as Kernel (OS)

    Note over W,T: Task Execution Level
    activate W
    W->>+T: 1. poll()
    T-->>-W: 2. return Poll::Pending
    
    Note over W,R: I/O Registration
    W->>+R: 3. register(fd, waker)
    W->>W: 4. park() or find next task
    deactivate W

    Note over R,K: Event Multiplexing (epoll/kqueue)
    R->>+K: 5. wait(timeout)
    K-->>-R: 6. Event Ready (FD)
    
    Note over R,T: Waker triggers re-scheduling
    R->>+T: 7. waker.wake()
    T->>W: 8. inject(task) into Scheduler
    deactivate T
    deactivate R
    
    Note over W,T: Task Completion
    activate W
    W->>+T: 9. poll() again
    T-->>-W: 10. return Poll::Ready(n)
    deactivate W
```

## Implementation Details

- **Memory**: Future allocation via Power-of-Two `SegQueue` buckets.
- **Timers**: Hashed wheel implementation for $O(1)$ timer management.
- **I/O**: Level-triggered multiplexing via the `polling` crate.
