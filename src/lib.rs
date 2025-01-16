#![forbid(unsafe_code)]
use std::rc::Rc;
use std::{cell::Cell, future::Future};

mod yielder {
    use std::{cell::Cell, rc::Rc, task::Poll};

    pub struct Yielder<Y> {
        pub(crate) sender: Sender<Y>,
    }

    impl<Y> Yielder<Y> {
        pub fn yield_value(&mut self, value: Y) -> impl Future<Output = ()> {
            YieldFuture {
                sender: &mut self.sender,
                value: Some(value),
            }
        }
    }

    // #[cfg(not(feature = "send"))]
    #[repr(transparent)]
    pub(crate) struct Sender<T>(pub(crate) Rc<Cell<Option<T>>>);

    impl<T> Sender<T> {
        pub(crate) fn send(&mut self, val: T) {
            let old = self.0.replace(Some(val));
            assert!(old.is_none(), "sending, but channel isn't empty!");
        }
    }

    #[repr(transparent)]
    pub(crate) struct Receiver<T>(pub(crate) Rc<Cell<Option<T>>>);

    impl<T> Receiver<T> {
        pub(crate) fn try_recv(&mut self) -> Option<T> {
            self.0.take()
        }
    }

    struct YieldFuture<'a, Y> {
        sender: &'a mut Sender<Y>,
        value: Option<Y>,
    }

    impl<Y> Unpin for YieldFuture<'_, Y> {}

    impl<Y> Future for YieldFuture<'_, Y> {
        type Output = ();

        fn poll(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            match self.value.take() {
                // First time being polled, yield so the caller (of the generator) regains control
                Some(v) => {
                    self.sender.send(v);
                    Poll::Pending
                }
                // Second time being polled
                None => Poll::Ready(()),
            }
        }
    }
}

mod generator {
    use std::{
        pin::Pin,
        sync::Arc,
        task::{Context, Poll, Wake},
    };

    use crate::yielder::*;

    struct NoopWake;
    impl Wake for NoopWake {
        fn wake(self: Arc<Self>) {}
    }

    pub(crate) use backend::MiniGen;
    mod backend {
        use std::{
            pin::Pin,
            sync::{Arc, LazyLock},
            task::{Context, Poll, Waker},
        };

        use crate::generator::NoopWake;

        use super::{GeneratorStatus, Receiver};

        pub struct MiniGen<'a, Y, R> {
            pub(crate) future: Pin<Box<dyn Future<Output = R> + 'a>>,
            pub(crate) receiver: Receiver<Y>,
            pub(crate) finished: bool,
        }

        impl<Y, R> MiniGen<'_, Y, R> {
            pub fn resume(&mut self) -> GeneratorStatus<Y, R> {
                // The future was polled to completion before, so we should act similar to a
                // fused iterator and return that it's completed
                if self.finished {
                    return GeneratorStatus::Completed;
                }
                // We don't actually need a proper "waker" so we can just sort of no-op it
                static WAKER: LazyLock<Waker> = LazyLock::new(|| Waker::from(Arc::new(NoopWake)));
                let mut context = Context::from_waker(&WAKER);
                loop {
                    let pinned = self.future.as_mut();
                    // We poll the future we're storing
                    match pinned.poll(&mut context) {
                        // Future has returned, generator has completed.
                        Poll::Ready(v) => {
                            self.finished = true;
                            return GeneratorStatus::Returned(v);
                        }
                        // Yield or a random await
                        Poll::Pending => {
                            // We check here, because the future might yield for some reason other than
                            // stream.yield_value()
                            if let Some(v) = self.receiver.try_recv() {
                                return GeneratorStatus::Yielded(v);
                            }
                        }
                    }
                }
            }

            pub fn resume_async(&mut self) -> impl Future<Output = GeneratorStatus<Y, R>> {
                MiniGenFuture {
                    generator: &mut *self,
                }
            }
        }

        struct MiniGenFuture<'r, 'g, Y, R> {
            generator: &'r mut MiniGen<'g, Y, R>,
        }

        impl<Y, R> Future for MiniGenFuture<'_, '_, Y, R> {
            type Output = GeneratorStatus<Y, R>;

            fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
                // The future was polled to completion before, so we should act similar to a
                // fused iterator and return that it's completed
                if self.generator.finished {
                    return Poll::Ready(GeneratorStatus::Completed);
                }
                let pinned = self.generator.future.as_mut();
                // We poll the future we're storing
                match pinned.poll(cx) {
                    // Future has returned, generator has completed.
                    Poll::Ready(v) => {
                        self.generator.finished = true;
                        Poll::Ready(GeneratorStatus::Returned(v))
                    }
                    // Yield or a random await
                    Poll::Pending => {
                        // We check here, because the future might yield for some reason other than
                        // stream.yield_value()
                        if let Some(v) = self.generator.receiver.try_recv() {
                            return Poll::Ready(GeneratorStatus::Yielded(v));
                        }
                        Poll::Pending
                    }
                }
            }
        }
    }

    pub struct MiniIter<'a, Y>(pub(crate) MiniGen<'a, Y, ()>);

    impl<Y> Iterator for MiniIter<'_, Y> {
        type Item = Y;
        fn next(&mut self) -> Option<Self::Item> {
            match self.0.resume() {
                GeneratorStatus::Yielded(it) => Some(it),
                GeneratorStatus::Returned(()) | GeneratorStatus::Completed => None,
            }
        }
    }

    impl<Y> Unpin for MiniIter<'_, Y> {}

    #[repr(transparent)]
    struct MiniIterFuture<'r, 'g, Y> {
        generator: &'r mut MiniIter<'g, Y>,
    }

    impl<Y: Send> MiniIter<'_, Y> {
        pub fn resume_async(&mut self) -> impl Future<Output = Option<Y>> {
            MiniIterFuture {
                generator: &mut *self,
            }
        }
    }

    impl<Y> Future for MiniIterFuture<'_, '_, Y> {
        type Output = Option<Y>;

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            // The future was polled to completion before, so we should act similar to a
            // fused iterator and return that it's completed
            if self.generator.0.finished {
                return Poll::Ready(None);
            }
            let pinned = self.generator.0.future.as_mut();
            // We poll the future we're storing
            match pinned.poll(cx) {
                // Future has returned, generator has completed.
                Poll::Ready(()) => {
                    self.generator.0.finished = true;
                    Poll::Ready(None)
                }
                // Yield or a random await
                Poll::Pending => {
                    // We check here, because the future might yield for some reason other than
                    // stream.yield_value()
                    if let Some(v) = self.generator.0.receiver.try_recv() {
                        return Poll::Ready(Some(v));
                    }
                    Poll::Pending
                }
            }
        }
    }

    #[cfg(feature = "stream")]
    impl<Y: Send> futures_core::Stream for MiniIter<'_, Y> {
        type Item = Y;

        fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            let fut = self.resume_async();
            let fut = std::pin::pin!(fut);
            fut.poll(cx)
        }
    }

    #[derive(Clone, Debug, Copy, PartialEq, Eq)]
    pub enum GeneratorStatus<Y, R> {
        Yielded(Y),
        Returned(R),
        Completed,
    }
}

pub use generator::*;
pub use yielder::*;

pub fn generator<'a, Y: Send, R, Fut: Future<Output = R> + 'a, F: FnOnce(Yielder<Y>) -> Fut>(
    func: F,
) -> MiniGen<'a, Y, R> {
    let inner = Rc::new(Cell::new(None));
    let (sender, receiver) = (Sender(inner.clone()), Receiver(inner));
    let stream = Yielder { sender };

    let future = func(stream);
    MiniGen {
        receiver,
        future: Box::pin(future),
        finished: false,
    }
}
pub fn iterator<'a, Y: Send, Fut: Future<Output = ()> + 'a, F: FnOnce(Yielder<Y>) -> Fut>(
    func: F,
) -> MiniIter<'a, Y> {
    MiniIter(generator(func))
}
