// Deferred 组件在 prepare 里读资源：ResourceSink 只能放、不能读
#![allow(unused_imports)]
use hygiea::{HyErr, Result};
use hygiea::app::{CancellationToken, DeferredComponent, Name, ReadyResources, ResourceId, ResourceSink, Resources, ImmediateComponent, component};
use tokio::task::JoinHandle;

#[derive(Clone)]
struct Pool;

struct Api;

#[component]
impl DeferredComponent for Api {
    type Config = ();

    fn build(_name: Name, _config: ()) -> Self {
        Self
    }

    async fn prepare(&mut self, sink: &ResourceSink) -> Result<()> {
        sink.require::<Pool>()?;
        Ok(())
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
