// 只接受 crate = "path"
#![allow(unused_imports)]
use hygiea::HyErr;
use hygiea::app::{CancellationToken, DeferredComponent, Name, ReadyResources, ResourceId, ResourceSink, Resources, ImmediateComponent, component};
use tokio::task::JoinHandle;

struct Db;

#[component(kind = "deferred")]
impl ImmediateComponent for Db {}

fn main() {}
