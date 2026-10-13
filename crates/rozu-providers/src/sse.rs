//! HTTP client setup and server-sent-event plumbing shared by the adapters. Each adapter supplies
//! a [`Decoder`] that turns one event's JSON payload into normalized [`StreamEvent`]s.

use std::time::Duration;

use eventsource_stream::Eventsource;
use futures::{Stream, StreamExt};
use reqwest::Response;
use rozu_core::{ProviderError, ProviderResult, StreamEvent};
use tokio_util::sync::CancellationToken;

/// A client with rozu's user agent and the given timeouts. `read_timeout` is the longest silence
/// tolerated between two chunks of a response.
pub(crate) fn http_client(
    connect_timeout: Duration,
    read_timeout: Duration,
) -> ProviderResult<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("rozu/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(connect_timeout)
        .read_timeout(read_timeout)
        .build()
        .map_err(|err| ProviderError::Network(err.to_string()))
}

/// Provider-specific decoding of SSE payloads.
pub(crate) trait Decoder: Send + 'static {
    /// Handles one event's `data`. An error ends the stream.
    fn handle(&mut self, data: &str) -> ProviderResult<Vec<StreamEvent>>;
    /// Whether the response is complete (the stream ends after the current batch).
    fn is_done(&self) -> bool;
}

/// Decodes `response`'s SSE body. Ends after the decoder reports done, on the first error, or
/// silently when `cancel` fires. A body that ends early yields [`ProviderError::UnexpectedEof`].
pub(crate) fn decode<D: Decoder>(
    response: Response,
    mut decoder: D,
    cancel: CancellationToken,
) -> impl Stream<Item = ProviderResult<StreamEvent>> + Send + 'static {
    async_stream::stream! {
        let mut events = response.bytes_stream().eventsource();
        loop {
            let next = tokio::select! {
                biased;
                _ = cancel.cancelled() => return,
                next = events.next() => next,
            };
            match next {
                None => {
                    yield Err(ProviderError::UnexpectedEof);
                    return;
                }
                Some(Err(err)) => {
                    yield Err(ProviderError::Network(err.to_string()));
                    return;
                }
                Some(Ok(event)) => {
                    let data = event.data.trim();
                    if data.is_empty() || data == "[DONE]" {
                        continue;
                    }
                    match decoder.handle(data) {
                        Ok(out) => {
                            for item in out {
                                yield Ok(item);
                            }
                            if decoder.is_done() {
                                return;
                            }
                        }
                        Err(err) => {
                            yield Err(err);
                            return;
                        }
                    }
                }
            }
        }
    }
}
