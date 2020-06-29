use std::cell::Cell;
use std::collections::VecDeque;
use std::io;
use std::os::unix::io::RawFd;
use std::rc::Rc;
use std::time::{Duration, Instant};

pub enum Event {
    Time(Instant),
    Readable(RawFd),
    Writable(RawFd),
    Join,
}

pub enum Poll<T> {
    Complete(T),
    WaitingFor(Event),
}

pub trait Future {
    type Output;
    fn poll(&mut self, runtime: &mut Runtime) -> Poll<Self::Output>;
}

impl Runtime {
    fn new() -> Self {
        Runtime {
            ready_tasks: VecDeque::new(),
        }
    }

    pub fn spawn<F, T>(&mut self, mut f: F) -> JoinHandle<T>
    where
        F: Future<Output = T> + 'static,
        T: 'static,
    {
        let shared_state = Rc::new(Cell::new(TaskState::Running(None)));
        let handle = JoinHandle {
            state: shared_state.clone(),
        };
        let progress_fn = Box::new(move |runtime: &mut Runtime| match f.poll(runtime) {
            Poll::Complete(value) => {
                match shared_state.replace(TaskState::Completed(value)) {
                    TaskState::Running(Some(t)) => runtime.ready_tasks.push_back(t),
                    TaskState::Running(None) => (),
                    _ => panic!("Task should be running"),
                }
                Poll::Complete(())
            }
            Poll::WaitingFor(e) => Poll::WaitingFor(e),
        });
        self.ready_tasks.push_back(progress_fn);
        handle
    }

    fn run(&mut self) {
        //
    }
}

pub struct JoinHandle<T> {
    state: Rc<Cell<TaskState<T>>>,
}

impl<T> JoinHandle<T> {
    ///
    pub fn try_join(self) -> Result<T, Self> {
        let state: &Cell<TaskState<T>> = &self.state;
        match state.replace(TaskState::Joined) {
            TaskState::Joined => panic!("Double join"),
            TaskState::Completed(value) => Ok(value),
            running => {
                state.set(running);
                Err(self)
            }
        }
    }
}

pub fn run<F, T>(f: F) -> Result<T, io::Error>
where
    F: Future<Output = T> + 'static,
    T: 'static,
{
    let mut runtime = Runtime {
        ready_tasks: VecDeque::new(),
    };
    let completion = runtime.spawn(f);
    runtime.run();
    completion
        .try_join()
        .map_err(|_| io::Error::from(io::ErrorKind::Other))
}

type BoxedProgressFn = Box<dyn FnMut(&mut Runtime) -> Poll<()>>;

pub struct Runtime {
    ready_tasks: VecDeque<BoxedProgressFn>,
}

enum TaskState<T> {
    Running(Option<BoxedProgressFn>), // Task waiting on self
    Completed(T),
    Joined,
}

fn sys_poll(fds: &mut [libc::pollfd], timeout: Option<Duration>) -> Result<usize, io::Error> {
    let return_code = unsafe {
        libc::ppoll(
            fds.as_mut_ptr(),
            fds.len() as libc::nfds_t,
            match timeout {
                None => std::ptr::null(),
                Some(t) => &libc::timespec {
                    tv_sec: t.as_secs() as libc::c_long,
                    tv_nsec: t.subsec_nanos() as libc::c_long,
                },
            },
            std::ptr::null(),
        )
    };
    match return_code {
        -1 => Err(io::Error::last_os_error()),
        n => Ok(n as usize),
    }
}

#[test]
fn test() {
    struct Ready<T>(Option<T>);
    impl<T> Ready<T> {
        fn new(t: T) -> Self {
            Ready(Some(t))
        }
    }
    impl<T> Future for Ready<T> {
        type Output = T;
        fn poll(&mut self, _runtime: &mut Runtime) -> Poll<T> {
            Poll::Complete(self.0.take().unwrap())
        }
    }

    assert_eq!(run(Ready::new(42)).map_err(|e| e.to_string()), Ok(42));

    enum Prog1 {Start,
        Spawned{a:JoinHandle<i32>, b:JoinHandle<i32>},
    }
    impl Future for Prog1 {
        type Output = i32;
        fn poll(&mut self, runtime: &mut Runtime) -> Poll<i32> {
            loop {
                *self = match *self {
                    Start => Prog1::Spawned{
                        a: runtime.spawn(Ready::new(42)),
                        b: runtime.spawn(Ready::new(-1))
                    }
                }
            }
        }
    }
}
