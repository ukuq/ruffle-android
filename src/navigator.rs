use async_channel::{Receiver, Sender};
use ruffle_core::backend::navigator::{
    ErrorResponse, NavigationMethod, NavigatorBackend, OwnedFuture, Request, SuccessResponse,
};
use ruffle_core::indexmap::IndexMap;
use ruffle_core::loader::Error;
use ruffle_core::socket::{SocketAction, SocketHandle};
use ruffle_frontend_utils::backends::navigator::{
    ExternalNavigatorBackend, FutureSpawner, NavigatorInterface,
};
use std::fs::File;
use std::path::Path;
use std::time::Duration;
use url::Url;

#[derive(Clone)]
pub struct AndroidNavigatorInterface;

impl NavigatorInterface for AndroidNavigatorInterface {
    fn navigate_to_website(&self, url: Url) {
        let _ = webbrowser::open(url.as_ref());
    }

    async fn open_file(&self, path: &Path) -> std::io::Result<File> {
        File::open(path)
    }

    async fn confirm_socket(&self, _host: &str, _port: u16) -> bool {
        true
    }
}

pub struct AndroidNavigatorBackend<F: FutureSpawner<Error>> {
    inner: ExternalNavigatorBackend<F, AndroidNavigatorInterface>,
}

impl<F: FutureSpawner<Error>> AndroidNavigatorBackend<F> {
    pub fn new(inner: ExternalNavigatorBackend<F, AndroidNavigatorInterface>) -> Self {
        Self { inner }
    }
}

impl<F: FutureSpawner<Error> + 'static> NavigatorBackend for AndroidNavigatorBackend<F> {
    fn navigate_to_url(
        &self,
        url: &str,
        target: &str,
        vars_method: Option<(NavigationMethod, IndexMap<String, String>)>,
    ) {
        let mut parsed_url = match self.resolve_url(url) {
            Ok(url) => url,
            Err(error) => {
                log::warn!("Could not parse navigation URL {url:?}: {error}");
                return;
            }
        };

        if let Some((_, query_pairs)) = vars_method {
            let mut modifier = parsed_url.query_pairs_mut();
            for (key, value) in query_pairs.iter() {
                modifier.append_pair(key, value);
            }
        }

        if parsed_url.scheme() == "javascript" {
            log::info!("Ignoring javascript navigation request: {parsed_url}");
            return;
        }

        log::info!("Opening web URL target={target}: {parsed_url}");
        match parsed_url.scheme() {
            "http" | "https" => crate::open_web_login_url(parsed_url.as_ref()),
            _ => self
                .inner
                .navigate_to_url(parsed_url.as_ref(), target, None),
        }
    }

    fn fetch(&self, request: Request) -> OwnedFuture<Box<dyn SuccessResponse>, ErrorResponse> {
        log::info!("Fetching {}", request.url());
        self.inner.fetch(request)
    }

    fn resolve_url(&self, url: &str) -> Result<Url, url::ParseError> {
        self.inner.resolve_url(url)
    }

    fn spawn_future(&mut self, future: OwnedFuture<(), Error>) {
        self.inner.spawn_future(future);
    }

    fn pre_process_url(&self, url: Url) -> Url {
        self.inner.pre_process_url(url)
    }

    fn connect_socket(
        &mut self,
        host: String,
        port: u16,
        timeout: Duration,
        handle: SocketHandle,
        receiver: Receiver<Vec<u8>>,
        sender: Sender<SocketAction>,
    ) {
        log::info!("Connecting socket {host}:{port}");
        let timeout = if timeout.is_zero() {
            Duration::from_secs(20)
        } else {
            timeout
        };
        self.inner
            .connect_socket(host, port, timeout, handle, receiver, sender);
    }
}
