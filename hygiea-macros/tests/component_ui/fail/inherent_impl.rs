// 标在不带 trait 的 impl 上
#![allow(unused_imports)]
use hygiea::HyErr;
use hygiea::app::{CancellationToken, DeferredComponent, Name, ReadyResources, ResourceId, ResourceSink, Resources, ImmediateComponent, component};
use tokio::task::JoinHandle;

struct Db;

#[component]
impl Db {}

fn main() {}
