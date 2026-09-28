// 手写 type Kind：由宏按 trait 填，不能自己写
#![allow(unused_imports)]
use hygiea::HyErr;
use hygiea::app::{CancellationToken, DeferredComponent, Name, ReadyResources, ResourceId, ResourceSink, Resources, ImmediateComponent, component};
use tokio::task::JoinHandle;

struct Db;

#[component]
impl ImmediateComponent for Db {
    type Config = ();
    type Kind = hygiea::app::Deferred;

    fn build(_name: Name, _config: ()) -> Self {
        Self
    }

    async fn startup(
        &mut self,
        _state: &Resources,
        _shutdown: CancellationToken,
    ) -> Result<Option<JoinHandle<()>>, HyErr> {
        Ok(None)
    }
}

fn main() {}
