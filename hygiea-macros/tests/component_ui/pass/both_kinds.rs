// Immediate / Deferred 各一个，都能 add 进同一个 Registry
use hygiea::HyErr;
use hygiea::app::{
    CancellationToken, DeferredComponent, Name, ReadyResources, Registry, ResourceId, ResourceSink,
    Resources, ImmediateComponent, component,
};
use tokio::task::JoinHandle;

#[derive(Clone)]
struct Pool;

struct Db;

#[component]
impl ImmediateComponent for Db {
    type Config = ();

    fn build(_name: Name, _config: ()) -> Self {
        Self
    }

    fn provides(&self) -> Vec<ResourceId> {
        vec![ResourceId::of::<Pool>()]
    }

    async fn stop(&mut self) -> Result<(), HyErr> {
        Ok(())
    }

    async fn startup(
        &mut self,
        state: &Resources,
        _shutdown: CancellationToken,
    ) -> Result<Option<JoinHandle<()>>, HyErr> {
        state.insert(Pool);
        Ok(None)
    }
}

struct Api;

// 带路径写 trait 名、显式指定 crate 也行
#[component(crate = "::hygiea")]
impl hygiea::app::DeferredComponent for Api {
    type Config = u16;

    fn build(_name: Name, _port: u16) -> Self {
        Self
    }

    fn depends_on(&self) -> Vec<ResourceId> {
        vec![ResourceId::of::<Pool>()]
    }

    fn shutdown_timeout(&self) -> Option<std::time::Duration> {
        None
    }

    async fn prepare(&mut self, _sink: &ResourceSink) -> Result<(), HyErr> {
        Ok(())
    }

    async fn activate(
        &mut self,
        resources: ReadyResources,
        _shutdown: CancellationToken,
    ) -> Result<Option<JoinHandle<()>>, HyErr> {
        resources.require::<Pool>()?;
        Ok(None)
    }
}

// DeferredComponent 引入了但上面用的是带路径的写法，这里引用一下免得 unused
fn _assert_deferred<T: DeferredComponent>() {}

fn main() {
    _assert_deferred::<Api>();
    let _registry = Registry::new().add::<Api>(8080).add::<Db>(());
}
