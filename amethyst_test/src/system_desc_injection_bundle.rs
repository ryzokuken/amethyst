use std::marker::PhantomData;

use amethyst::{
    core::{bundle::SystemBundle, SystemDesc},
    ecs::prelude::*,
    error::Error,
};

use derive_new::new;

/// Adds a specified `System` to the dispatcher.
#[derive(Debug, new)]
pub(crate) struct SystemDescInjectionBundle<'a, 'b, SD, S>
where
    SD: SystemDesc<'a, 'b, S>,
    S: for<'s> System<'s> + Send,
{
    /// Function to instantiate `System` to add to the dispatcher.
    system_desc: SD,
    /// Name to register the system with.
    system_name: &'static str,
    /// Names of the system dependencies.
    system_dependencies: &'static [&'static str],
    /// Marker.
    system_marker: PhantomData<(&'a SD, &'b S)>,
}

impl<'a, 'b, SD, S> SystemBundle<'a, 'b> for SystemDescInjectionBundle<'a, 'b, SD, S>
where
    SD: SystemDesc<'a, 'b, S>,
    S: for<'s> System<'s> + Send + 'a,
{
    fn build(
        self,
        world: &mut World,
        builder: &mut DispatcherBuilder<'a, 'b>,
    ) -> Result<(), Error> {
        builder.add(
            self.system_desc.build(world),
            self.system_name,
            self.system_dependencies,
        );
        Ok(())
    }
}
