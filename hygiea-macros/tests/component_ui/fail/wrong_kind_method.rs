// Immediate 组件写了 Deferred 的 activate
#![allow(unused_imports)]
use hygiea::{HyErr, Result};
use hygiea::app::{CancellationToken, DeferredComponent, Name, ReadyResources, ResourceId, ResourceSink, Resources, ImmediateComponent, component};
use tokio::task::JoinHandle;

struct Db;

#[component]
impl ImmediateComponent for Db {
    type Config = ();

    fn build(_name: Name, _config: ()) -> Self {
        Self
    }

    async fn activate(
        &mut self,
        _resources: ReadyResources,
        _shutdown: CancellationToken,
    ) -> Result<Option<JoinHandle<()>>> {
        Ok(None)
    }
}

fn main() {}
