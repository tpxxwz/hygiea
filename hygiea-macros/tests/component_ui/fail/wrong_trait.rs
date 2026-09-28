// 标在别的 trait 的 impl 上
#![allow(unused_imports)]
use hygiea::HyErr;
use hygiea::app::{CancellationToken, DeferredComponent, Name, ReadyResources, ResourceId, ResourceSink, Resources, ImmediateComponent, component};
use tokio::task::JoinHandle;

trait Other {}

struct Db;

#[component]
impl Other for Db {}

fn main() {}
