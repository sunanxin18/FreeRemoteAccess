use std::sync::Arc;

use raw_window_handle::{HasDisplayHandle, HasWindowHandle};

use frd_render_wgpu::{GpuContext, GpuContextError};

use crate::{state::OwnedSurfaceAndLease, PresentError};

pub trait OwnedWindowTarget: HasDisplayHandle + HasWindowHandle + Send + Sync + 'static {}

impl<T> OwnedWindowTarget for T where T: HasDisplayHandle + HasWindowHandle + Send + Sync + 'static {}

#[derive(Clone)]
pub struct PresentationSurfaceLease {
    owner: Arc<dyn OwnedWindowTarget>,
}

impl PresentationSurfaceLease {
    pub fn new<T>(owner: Arc<T>) -> Self
    where
        T: OwnedWindowTarget,
    {
        Self { owner }
    }

    fn target(&self) -> Arc<dyn OwnedWindowTarget> {
        self.owner.clone()
    }
}

pub struct PresentationSurface {
    owned: OwnedSurfaceAndLease<wgpu::Surface<'static>, PresentationSurfaceLease>,
}

impl PresentationSurface {
    pub fn create(
        instance: &wgpu::Instance,
        lease: PresentationSurfaceLease,
    ) -> Result<Self, PresentError> {
        let surface = instance
            .create_surface(lease.target())
            .map_err(|_| PresentError::SurfaceCreation)?;
        Ok(Self {
            owned: OwnedSurfaceAndLease::new(surface, lease),
        })
    }

    pub async fn request_gpu_context(
        &self,
        instance: wgpu::Instance,
    ) -> Result<GpuContext, GpuContextError> {
        GpuContext::request(instance, self.surface()).await
    }

    pub(crate) fn surface(&self) -> Option<&wgpu::Surface<'static>> {
        self.owned.surface()
    }

    pub(crate) fn recreate(&mut self, instance: &wgpu::Instance) -> Result<(), PresentError> {
        self.owned.drop_surface();
        let target = self
            .owned
            .lease()
            .ok_or(PresentError::SurfaceDetached)?
            .target();
        let surface = instance
            .create_surface(target)
            .map_err(|_| PresentError::SurfaceCreation)?;
        self.owned.replace_surface(surface);
        Ok(())
    }

    pub fn detach(&mut self) {
        self.owned.detach();
    }
}
