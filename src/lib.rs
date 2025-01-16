#![forbid(unsafe_code)]
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, LazyLock,
        mpsc::{Receiver, Sender, channel},
    },
    task::{Context, Poll, Wake, Waker},
};

pub struct Yielder<Y> {
    sender: Sender<Y>,
}

impl<Y> Yielder<Y> {
    pub fn yield_value(&mut self, value: Y) -> impl Future<Output = ()> {
        YieldFuture {
            sender: &self.sender,
            value: Some(value),
        }
    }
}

struct YieldFuture<'a, Y> {
    sender: &'a Sender<Y>,
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
            Some(v) => {
                self.sender.send(v).unwrap();
                Poll::Pending
            }
            None => Poll::Ready(()),
        }
    }
}

struct NoopWake;
impl Wake for NoopWake {
    fn wake(self: Arc<Self>) {}
}

pub struct MiniGen<'a, Y, R> {
    future: Pin<Box<dyn Future<Output = R> + 'a>>,
    receiver: Receiver<Y>,
    finished: bool,
}

#[repr(transparent)]
pub struct MiniIter<'a, Y>(MiniGen<'a, Y, ()>);

impl<Y> Iterator for MiniIter<'_, Y> {
    type Item = Y;
    fn next(&mut self) -> Option<Self::Item> {
        match self.0.resume() {
            GeneratorStatus::Yielded(it) => Some(it),
            GeneratorStatus::Returned(()) | GeneratorStatus::Completed => None,
        }
    }
}

impl<Y> MiniIter<'_, Y> {
    pub fn resume(&mut self) -> impl Future<Output = Option<Y>> {
        MiniIterFuture {
            generator: &mut *self,
        }
    }
}

struct MiniIterFuture<'r, 'g, Y> {
    generator: &'r mut MiniIter<'g, Y>,
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
                if let Ok(v) = self.generator.0.receiver.try_recv() {
                    return Poll::Ready(Some(v));
                }
                Poll::Pending
            }
        }
    }
}

#[cfg(feature = "stream")]
impl<Y> futures_core::Stream for MiniIter<'_, Y> {
    type Item = Y;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let fut = self.resume();
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
                    if let Ok(v) = self.receiver.try_recv() {
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
                if let Ok(v) = self.generator.receiver.try_recv() {
                    return Poll::Ready(GeneratorStatus::Yielded(v));
                }
                Poll::Pending
            }
        }
    }
}

pub fn generator<'a, Y, R, Fut: Future<Output = R> + 'a, F: FnOnce(Yielder<Y>) -> Fut>(
    func: F,
) -> MiniGen<'a, Y, R> {
    let (sender, receiver) = channel();
    let stream = Yielder::<Y> { sender };

    let future = func(stream);
    MiniGen {
        receiver,
        future: Box::pin(future),
        finished: false,
    }
}

pub fn iterator<'a, Y, Fut: Future<Output = ()> + 'a, F: FnOnce(Yielder<Y>) -> Fut>(
    func: F,
) -> MiniIter<'a, Y> {
    MiniIter(generator(func))
}
