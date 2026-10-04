// 不用宏、手写两个 impl：Kind 写成 Deferred 却实现的是 ImmediateComponent，编译不过
#![allow(unused_imports)]
use hygiea::{HyErr, Result};
use hygiea::app::{CancellationToken, DeferredComponent, Name, ReadyResources, ResourceId, ResourceSink, Resources, ImmediateComponent, component};
use tokio::task::JoinHandle;

struct Db;

impl hygiea::app::Component for Db {
    type Config = ();
    type Kind = hygiea::app::Deferred;

    fn build(_name: Name, _config: ()) -> Self {
        Self
    }
}

#[hygiea::app::async_trait]
impl ImmediateComponent for Db {
    async fn startup(
        &mut self,
        _state: &Resources,
        _shutdown: CancellationToken,
    ) -> Result<Option<JoinHandle<()>>> {
        Ok(None)
    }
}

fn main() {}
