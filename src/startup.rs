use futures::{StreamExt, stream::FuturesUnordered};
use lump_core::{
    error::LumpUnknownError,
    resources::LocalResource,
    schedule::{ScheduleConfigure, ScheduleLabel, SystemsMap},
    world::{SystemId, WorldCenter, WorldState, WorldSystemLockError},
};

use crate::runtime::{AsyncRuntime, RuntimeConfig, SystemHandle};

#[derive(Default)]
struct StartupSystems {
    systems: SystemsMap<(), Result<(), LumpUnknownError>>,
    pendings: Vec<SystemId>,
}

impl LocalResource for StartupSystems {}

#[derive(Copy, Clone)]
pub struct Startup;

impl ScheduleLabel for Startup {}

impl ScheduleConfigure<(), Result<(), LumpUnknownError>> for Startup {
    fn add(
        world: &mut lump_core::world::World,
        systemid: lump_core::world::SystemId,
        system: lump_core::prelude::DynSystem<(), Result<(), LumpUnknownError>>,
    ) {
        let systems = world
            .center
            .resources
            .get_mut::<StartupSystems>()
            .expect("Startup schedule was not initialized");

        systems.systems.add_system(systemid, system);
        systems.pendings.push(systemid);
    }
}

impl Startup {
    pub fn init(world: &mut WorldCenter) {
        world.resources.init::<StartupSystems>();
    }

    pub fn create_invoker<'w, C: RuntimeConfig>(
        center: &'w mut WorldCenter,
        state: &'w mut WorldState,
        rt: &'w C::AsyncRuntime,
    ) -> StartupInvoke<'w, C> {
        let systems = center
            .resources
            .try_take::<StartupSystems>()
            .expect("Startup schedule was not initialized");

        StartupInvoke {
            center,
            rt,
            state,
            systems,
            futures: FuturesUnordered::new(),
        }
    }
}

pub struct StartupInvoke<'w, C: RuntimeConfig> {
    center: &'w mut WorldCenter,
    rt: &'w C::AsyncRuntime,
    state: &'w mut WorldState,
    systems: StartupSystems,
    futures: FuturesUnordered<SystemHandle<C>>,
}

impl<'w, C: RuntimeConfig> StartupInvoke<'w, C> {
    fn collect_pending_systems(&mut self) {
        let Self {
            center,
            rt,
            state,
            systems,
            futures,
        } = self;

        for _ in systems.pendings.extract_if(.., |id| {
            let id = *id;
            let system = match systems.systems.get(id) {
                Some(system) => system,
                None => return false,
            };

            match center.system_locks.try_lock(id) {
                Ok(_) => {}
                Err(WorldSystemLockError::NotRegistered) => {
                    panic!("System not registered")
                }
                Err(WorldSystemLockError::InvalidAccess) => return false,
            };

            let fut = system.run(state, ());
            let fut = rt.spawn(async move {
                let _ = fut.await;
                id
            });

            futures.push(fut);

            true
        }) {}
    }

    pub async fn invoke(mut self) {
        while let Some(systemid) = self.futures.next().await {
            self.center.system_locks.release(systemid);
            self.center.tick_commands(self.state);

            self.collect_pending_systems();
        }
    }
}
