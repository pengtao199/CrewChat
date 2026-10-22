use std::sync::Arc;

use crate::config::Config;
use crate::store::Store;
use crate::upstream::Upstream;

#[derive(Clone)]
pub struct App {
    pub config: Arc<Config>,
    pub store: Store,
    pub upstream: Arc<Upstream>,
}
