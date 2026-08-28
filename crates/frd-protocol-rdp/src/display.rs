use frd_core::{PhysicalViewport, PixelSize};
use ironrdp::displaycontrol::pdu::MonitorLayoutEntry;

use crate::surface::validate_surface_size;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResizeConfirmation {
    NoRequest,
    Confirmed,
    Mismatch,
}

pub(crate) struct DisplayControlAdapter {
    negotiated: bool,
    confirmed_size: PixelSize,
    queued: Option<PixelSize>,
    in_flight: Option<PixelSize>,
}

impl DisplayControlAdapter {
    pub(crate) fn new(confirmed_size: PixelSize) -> Self {
        Self {
            negotiated: false,
            confirmed_size,
            queued: None,
            in_flight: None,
        }
    }

    pub(crate) fn dynamic_resolution(&self) -> bool {
        self.negotiated
    }

    #[cfg(test)]
    fn confirmed_size(&self) -> PixelSize {
        self.confirmed_size
    }

    pub(crate) fn set_negotiated(&mut self, negotiated: bool) {
        self.negotiated = negotiated;
        if !negotiated && self.in_flight.is_none() {
            self.queued = None;
        }
    }

    pub(crate) fn observe_viewport(&mut self, viewport: PhysicalViewport) {
        if !self.negotiated && self.in_flight.is_none() {
            return;
        }
        let (width, height) = MonitorLayoutEntry::adjust_display_size(
            viewport.content.width,
            viewport.content.height,
        );
        let target = PixelSize { width, height };
        if validate_surface_size(target).is_err() {
            self.queued = None;
            return;
        }
        if let Some(in_flight) = self.in_flight {
            self.queued = (target != in_flight).then_some(target);
        } else if target == self.confirmed_size {
            self.queued = None;
        } else {
            self.queued = Some(target);
        }
    }

    pub(crate) fn take_resize_request(&mut self) -> Option<PixelSize> {
        if !self.negotiated || self.in_flight.is_some() {
            return None;
        }
        let target = self.queued.take()?;
        self.in_flight = Some(target);
        Some(target)
    }

    pub(crate) fn confirm_reactivation(&mut self, observed: PixelSize) -> ResizeConfirmation {
        match self.in_flight {
            None => {
                self.confirmed_size = observed;
                ResizeConfirmation::NoRequest
            }
            Some(expected) if expected == observed => {
                self.confirmed_size = observed;
                self.in_flight = None;
                ResizeConfirmation::Confirmed
            }
            Some(_) => ResizeConfirmation::Mismatch,
        }
    }
}

#[cfg(test)]
mod tests {
    use frd_core::{PhysicalViewport, PixelRect, PixelSize};

    use super::{DisplayControlAdapter, ResizeConfirmation};

    fn viewport(width: u32, height: u32) -> PhysicalViewport {
        let size = PixelSize { width, height };
        PhysicalViewport::new(
            size,
            PixelRect {
                x: 0,
                y: 0,
                width,
                height,
            },
            PixelSize {
                width: 1280,
                height: 720,
            },
        )
        .expect("valid viewport")
    }

    #[test]
    fn display_unnegotiated_viewport_is_ignored_without_changing_confirmed_size() {
        let initial = PixelSize {
            width: 1280,
            height: 720,
        };
        let mut adapter = DisplayControlAdapter::new(initial);

        adapter.observe_viewport(viewport(1600, 900));

        assert!(!adapter.dynamic_resolution());
        assert_eq!(adapter.take_resize_request(), None);
        assert_eq!(adapter.confirmed_size(), initial);
    }

    #[test]
    fn display_coalesces_requests_and_confirms_only_the_exact_reactivation() {
        let mut adapter = DisplayControlAdapter::new(PixelSize {
            width: 1280,
            height: 720,
        });
        adapter.set_negotiated(true);
        adapter.observe_viewport(viewport(1601, 900));

        assert_eq!(
            adapter.take_resize_request(),
            Some(PixelSize {
                width: 1600,
                height: 900,
            })
        );
        adapter.observe_viewport(viewport(1920, 1080));
        assert_eq!(adapter.take_resize_request(), None);
        assert_eq!(
            adapter.confirm_reactivation(PixelSize {
                width: 1598,
                height: 900,
            }),
            ResizeConfirmation::Mismatch
        );
        assert_eq!(
            adapter.confirmed_size(),
            PixelSize {
                width: 1280,
                height: 720,
            }
        );
        assert_eq!(
            adapter.confirm_reactivation(PixelSize {
                width: 1600,
                height: 900,
            }),
            ResizeConfirmation::Confirmed
        );
        assert_eq!(
            adapter.take_resize_request(),
            Some(PixelSize {
                width: 1920,
                height: 1080,
            })
        );
    }

    #[test]
    fn display_latest_viewport_can_cancel_a_queued_follow_up() {
        let mut adapter = DisplayControlAdapter::new(PixelSize {
            width: 1280,
            height: 720,
        });
        adapter.set_negotiated(true);
        adapter.observe_viewport(viewport(1600, 900));
        assert_eq!(
            adapter.take_resize_request(),
            Some(PixelSize {
                width: 1600,
                height: 900,
            })
        );
        adapter.observe_viewport(viewport(1920, 1080));
        adapter.observe_viewport(viewport(1601, 900));
        assert_eq!(
            adapter.confirm_reactivation(PixelSize {
                width: 1600,
                height: 900,
            }),
            ResizeConfirmation::Confirmed
        );
        assert_eq!(adapter.take_resize_request(), None);
    }

    #[test]
    fn display_return_to_preflight_size_remains_a_follow_up_after_exact_commit() {
        let initial = PixelSize {
            width: 1280,
            height: 720,
        };
        let mut adapter = DisplayControlAdapter::new(initial);
        adapter.set_negotiated(true);
        adapter.observe_viewport(viewport(1600, 900));
        assert_eq!(
            adapter.take_resize_request(),
            Some(PixelSize {
                width: 1600,
                height: 900,
            })
        );

        adapter.observe_viewport(viewport(1280, 720));
        assert_eq!(
            adapter.confirm_reactivation(PixelSize {
                width: 1600,
                height: 900,
            }),
            ResizeConfirmation::Confirmed
        );
        assert_eq!(adapter.take_resize_request(), Some(initial));
    }

    #[test]
    fn display_dvc_loss_preserves_the_exact_in_flight_ack_guard() {
        let initial = PixelSize {
            width: 1280,
            height: 720,
        };
        let mut adapter = DisplayControlAdapter::new(initial);
        adapter.set_negotiated(true);
        adapter.observe_viewport(viewport(1600, 900));
        assert_eq!(
            adapter.take_resize_request(),
            Some(PixelSize {
                width: 1600,
                height: 900,
            })
        );

        adapter.set_negotiated(false);
        assert_eq!(
            adapter.confirm_reactivation(PixelSize {
                width: 1280,
                height: 720,
            }),
            ResizeConfirmation::Mismatch
        );
        assert_eq!(adapter.confirmed_size(), initial);
    }

    #[test]
    fn display_surface_budget_accepts_exact_limit_and_ignores_over_budget_target() {
        let initial = PixelSize {
            width: 1280,
            height: 720,
        };
        let mut exact = DisplayControlAdapter::new(initial);
        exact.set_negotiated(true);
        exact.observe_viewport(viewport(8192, 2048));
        assert_eq!(
            exact.take_resize_request(),
            Some(PixelSize {
                width: 8192,
                height: 2048,
            })
        );

        let mut over_budget = DisplayControlAdapter::new(initial);
        over_budget.set_negotiated(true);
        over_budget.observe_viewport(viewport(8192, 2049));
        assert_eq!(over_budget.take_resize_request(), None);
        assert_eq!(over_budget.confirmed_size(), initial);
    }
}
