use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::Future;
use std::io;
use std::os::unix::io::RawFd;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct RuntimeHandle(Rc<RefCell<Runtime>>);

struct Runtime {
    ready_tasks: VecDeque<Pin<Rc<dyn TaskMakeProgress>>>,
}

impl RuntimeHandle {
    fn new() -> Self {
        Self(Rc::new(RefCell::new(Runtime {
            ready_tasks: VecDeque::new(),
        })))
    }

    pub fn spawn<F, R>(&self, f: F) -> JoinHandle<R>
    where
        F: Future<Output = R> + 'static,
        R: 'static,
    {
        let task = Rc::pin(RefCell::new(TaskState::Running {
            future: f,
            wake_on_completion: None,
        }));
        self.0.borrow_mut().ready_tasks.push_back(task.clone());
        JoinHandle(task)
    }

    pub fn block_on<F, R>(&self, f: F) -> Result<R, io::Error>
    where
        F: Future<Output = R> + 'static,
        R: 'static,
    {
        let handle = self.spawn(f);
        while let Some(task) = self.0.borrow_mut().ready_tasks.pop_front() {
            task.as_ref().make_progress()
        }
        unimplemented!()
    }
}

/// Task frame ; holds the future then the future's result.
/// This is allocated once in an Rc.
/// One handle is given to the runtime with the make_progress capability.
/// Another handle is given to the user to get the return value (JoinHandle).
enum TaskState<F, R> {
    Running {
        future: F, // Only future is pin-structural
        wake_on_completion: Option<Waker>,
    },
    Completed(Option<R>),
}

/// Internal trait for the make_progress capability.
/// Used to get a type erased reference to the task for the runtime.
trait TaskMakeProgress {
    fn make_progress(self: Pin<&Self>);
}

impl<F, R> TaskMakeProgress for RefCell<TaskState<F, R>>
where
    F: Future<Output = R>,
{
    fn make_progress(self: Pin<&Self>) {
        let (future, wake_on_completion) = match &mut *self.as_ref().borrow_mut() {
            TaskState::Running {
                future,
                wake_on_completion,
            } => (
                // SAFETY : The future is not moved out until destruction when completed
                unsafe { Pin::new_unchecked(future) },
                wake_on_completion,
            ),
            TaskState::Completed(_) => panic!("Running completed task"),
        };
        let mut context = Context::from_waker(unimplemented!());
        match future.poll(&mut context) {
            Poll::Pending => (),
            Poll::Ready(value) => {
                if let Some(waker) = wake_on_completion {
                    waker.wake()
                }
                *self.borrow_mut() = TaskState::Completed(Some(value))
            }
        }
    }
}

/// Internal trait for the testing task completion and return value extraction.
/// Allows a partially type erased (remove the F, keep the R) reference to the task frame.
trait TaskJoin {
    type Output;
    fn join(&self, waker: &Waker) -> Poll<Self::Output>;
}

impl<F, T> TaskJoin for RefCell<TaskState<F, T>>
where
    F: Future<Output = T>,
{
    type Output = T;
    fn join(&self, waker: &Waker) -> Poll<T> {
        match &mut *self.borrow_mut() {
            TaskState::Running {
                future: _, // SAFETY : future is not moved
                wake_on_completion,
            } => {
                *wake_on_completion = Some(waker.clone()); // Always update waker
                Poll::Pending
            }
            TaskState::Completed(value) => Poll::Ready(value.take().expect("Double join")),
        }
    }
}

/// Creating a task returns this JoinHandle, which represents the task completion.
pub struct JoinHandle<T>(Pin<Rc<dyn TaskJoin<Output = T>>>);

impl<T> Future for JoinHandle<T> {
    type Output = T;
    fn poll(self: Pin<&mut Self>, context: &mut Context) -> Poll<T> {
        self.0.join(context.waker())
    }
}

// Wrap poll() syscall
fn syscall_poll(fds: &mut [libc::pollfd], timeout: Option<Duration>) -> Result<usize, io::Error> {
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
    let runtime = RuntimeHandle::new();
    let r = runtime.block_on(async { 42 }).expect("no sys error");
    assert_eq!(r, 42);
}
