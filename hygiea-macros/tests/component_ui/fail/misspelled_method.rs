// provides 拼成 provide：不是 Component 的项，留在 ImmediateComponent 的 impl 里，报在这个方法上
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

    fn provide(&self) -> Vec<ResourceId> {
        Vec::new()
    }

    async fn startup(
        &mut self,
        _state: &Resources,
        _shutdown: CancellationToken,
    ) -> Result<Option<JoinHandle<()>>> {
        Ok(None)
    }
}

fn main() {}
