// 漏写 build：报在类型名上
#![allow(unused_imports)]
use hygiea::{HyErr, Result};
use hygiea::app::{CancellationToken, DeferredComponent, Name, ReadyResources, ResourceId, ResourceSink, Resources, ImmediateComponent, component};
use tokio::task::JoinHandle;

struct Db;

#[component]
impl ImmediateComponent for Db {
    type Config = ();

    async fn startup(
        &mut self,
        _state: &Resources,
        _shutdown: CancellationToken,
    ) -> Result<Option<JoinHandle<()>>> {
        Ok(None)
    }
}

fn main() {}
