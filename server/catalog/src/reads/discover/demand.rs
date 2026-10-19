//! The discover content's warm pass, as the jobs warm-cycle loop drives it.

use std::sync::Arc;

use super::ports::DiscoverContent;
use droppedneedle_kernel::jobs::BoxFuture;
use droppedneedle_kernel::jobs::discovery_demand::DemandTick;

/// The discover content's warm pass.
pub struct ContentDemand(pub Arc<dyn DiscoverContent>);

impl DemandTick for ContentDemand {
    fn tick(&self) -> BoxFuture<'_, ()> {
        self.0.run_due_tick()
    }
}
