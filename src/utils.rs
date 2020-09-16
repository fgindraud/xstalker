pub use try_join::TryJoin;

mod try_join {
    use pin_project::pin_project;
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    #[pin_project]
    pub struct TryJoin<F0, F1> {
        #[pin]
        f0: Option<F0>,
        #[pin]
        f1: Option<F1>,
    }

    impl<F0, F1> TryJoin<F0, F1> {
        pub fn new(f0: F0, f1: F1) -> Self {
            Self {
                f0: Some(f0),
                f1: Some(f1),
            }
        }
    }

    /// Run all futures.
    /// Exit early with error if one of them has an error.
    /// If all future complete with no error, return ().
    impl<F0, F1, E> Future for TryJoin<F0, F1>
    where
        F0: Future<Output = Result<(), E>>,
        F1: Future<Output = Result<(), E>>,
    {
        type Output = Result<(), E>;

        fn poll(self: Pin<&mut Self>, context: &mut Context) -> Poll<Self::Output> {
            let proj = self.project();
            let mut all_complete = true;
            match advance_to_completion(proj.f0, context) {
                Ok(complete) => all_complete = all_complete && complete,
                Err(e) => return Poll::Ready(Err(e)),
            }
            match advance_to_completion(proj.f1, context) {
                Ok(complete) => all_complete = all_complete && complete,
                Err(e) => return Poll::Ready(Err(e)),
            }
            match all_complete {
                true => Poll::Ready(Ok(())),
                false => Poll::Pending,
            }
        }
    }

    fn advance_to_completion<F, E>(
        mut maybe_fut: Pin<&mut Option<F>>,
        context: &mut Context,
    ) -> Result<bool, E>
    where
        F: Future<Output = Result<(), E>>,
    {
        match maybe_fut.as_mut().as_pin_mut() {
            Some(fut) => match fut.poll(context) {
                Poll::Ready(r) => {
                    maybe_fut.set(None);
                    r.map(|()| true)
                }
                Poll::Pending => Ok(false),
            },
            None => Ok(true),
        }
    }
}
