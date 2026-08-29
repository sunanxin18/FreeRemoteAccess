use std::sync::{Arc, Mutex};

use frd_core::{PhysicalViewport, PixelSize};
use ironrdp::displaycontrol::pdu::{DisplayControlCapabilities, MonitorLayoutEntry};

use crate::surface::validate_surface_size;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResizeConfirmation {
    NoRequest,
    Confirmed,
    Mismatch,
}

#[derive(Clone, Default)]
pub(crate) struct DisplayControlCapabilityState {
    max_monitor_area: Arc<Mutex<Option<u64>>>,
}

impl DisplayControlCapabilityState {
    pub(crate) fn record(&self, capabilities: &DisplayControlCapabilities) {
        *self
            .max_monitor_area
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(capabilities.max_monitor_area());
    }

    pub(crate) fn max_monitor_area(&self) -> Option<u64> {
        *self
            .max_monitor_area
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

pub(crate) struct DisplayControlAdapter {
    max_monitor_area: Option<u64>,
    confirmed_size: PixelSize,
    queued: Option<PixelSize>,
    in_flight: Option<PixelSize>,
}

impl DisplayControlAdapter {
    pub(crate) fn new(confirmed_size: PixelSize) -> Self {
        Self {
            max_monitor_area: None,
            confirmed_size,
            queued: None,
            in_flight: None,
        }
    }

    pub(crate) fn dynamic_resolution(&self) -> bool {
        self.max_monitor_area.is_some()
    }

    #[cfg(test)]
    fn confirmed_size(&self) -> PixelSize {
        self.confirmed_size
    }

    pub(crate) fn set_negotiated(&mut self, max_monitor_area: Option<u64>) {
        self.max_monitor_area = max_monitor_area;
        if max_monitor_area.is_none() && self.in_flight.is_none() {
            self.queued = None;
        }
        if self
            .queued
            .is_some_and(|target| !self.within_server_area(target))
        {
            self.queued = None;
        }
    }

    pub(crate) fn observe_viewport(&mut self, viewport: PhysicalViewport) {
        if self.max_monitor_area.is_none() && self.in_flight.is_none() {
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
        if self
            .max_monitor_area
            .is_some_and(|_| !self.within_server_area(target))
        {
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
        if self.max_monitor_area.is_none() || self.in_flight.is_some() {
            return None;
        }
        let target = self.queued.take()?;
        if !self.within_server_area(target) {
            return None;
        }
        self.in_flight = Some(target);
        Some(target)
    }

    fn within_server_area(&self, target: PixelSize) -> bool {
        self.max_monitor_area.is_some_and(|max_monitor_area| {
            u64::from(target.width) * u64::from(target.height) <= max_monitor_area
        })
    }

    pub(crate) fn confirm_reactivation(
        &mut self,
        observed: PixelSize,
        current_max_monitor_area: Option<u64>,
    ) -> ResizeConfirmation {
        self.set_negotiated(current_max_monitor_area);
        match self.in_flight {
            None => {
                self.confirmed_size = observed;
                ResizeConfirmation::NoRequest
            }
            Some(expected) if !self.within_server_area(expected) => {
                self.in_flight = None;
                ResizeConfirmation::Mismatch
            }
            Some(expected) if expected != observed => ResizeConfirmation::Mismatch,
            Some(_) => {
                self.confirmed_size = observed;
                self.in_flight = None;
                ResizeConfirmation::Confirmed
            }
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
        adapter.set_negotiated(Some(u64::MAX));
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
            adapter.confirm_reactivation(
                PixelSize {
                    width: 1598,
                    height: 900,
                },
                Some(u64::MAX),
            ),
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
            adapter.confirm_reactivation(
                PixelSize {
                    width: 1600,
                    height: 900,
                },
                Some(u64::MAX),
            ),
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
        adapter.set_negotiated(Some(u64::MAX));
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
            adapter.confirm_reactivation(
                PixelSize {
                    width: 1600,
                    height: 900,
                },
                Some(u64::MAX),
            ),
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
        adapter.set_negotiated(Some(u64::MAX));
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
            adapter.confirm_reactivation(
                PixelSize {
                    width: 1600,
                    height: 900,
                },
                Some(u64::MAX),
            ),
            ResizeConfirmation::Confirmed
        );
        assert_eq!(adapter.take_resize_request(), Some(initial));
    }

    #[test]
    fn display_dvc_loss_rejects_and_retires_an_exact_in_flight_reactivation() {
        let initial = PixelSize {
            width: 1280,
            height: 720,
        };
        let mut adapter = DisplayControlAdapter::new(initial);
        adapter.set_negotiated(Some(u64::MAX));
        adapter.observe_viewport(viewport(1600, 900));
        assert_eq!(
            adapter.take_resize_request(),
            Some(PixelSize {
                width: 1600,
                height: 900,
            })
        );

        assert_eq!(
            adapter.confirm_reactivation(
                PixelSize {
                    width: 1600,
                    height: 900,
                },
                None,
            ),
            ResizeConfirmation::Mismatch
        );
        assert_eq!(adapter.confirmed_size(), initial);

        adapter.set_negotiated(Some(1280 * 720));
        adapter.observe_viewport(viewport(1024, 768));
        assert_eq!(
            adapter.take_resize_request(),
            Some(PixelSize {
                width: 1024,
                height: 768,
            })
        );
        assert_eq!(
            adapter.confirm_reactivation(
                PixelSize {
                    width: 1024,
                    height: 768,
                },
                Some(1280 * 720),
            ),
            ResizeConfirmation::Confirmed
        );
    }

    #[test]
    fn display_capability_shrink_rejects_stale_exact_ack_and_preserves_latest_valid_target() {
        let initial = PixelSize {
            width: 800,
            height: 600,
        };
        let mut adapter = DisplayControlAdapter::new(initial);
        adapter.set_negotiated(Some(1600 * 900));
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
            adapter.confirm_reactivation(
                PixelSize {
                    width: 1600,
                    height: 900,
                },
                Some(1280 * 720),
            ),
            ResizeConfirmation::Mismatch
        );
        assert_eq!(adapter.confirmed_size(), initial);
        assert_eq!(
            adapter.take_resize_request(),
            Some(PixelSize {
                width: 1280,
                height: 720,
            })
        );
        assert_eq!(
            adapter.confirm_reactivation(
                PixelSize {
                    width: 1280,
                    height: 720,
                },
                Some(1280 * 720),
            ),
            ResizeConfirmation::Confirmed
        );
        assert_eq!(
            adapter.confirmed_size(),
            PixelSize {
                width: 1280,
                height: 720,
            }
        );
    }

    #[test]
    fn display_surface_budget_accepts_exact_limit_and_ignores_over_budget_target() {
        let initial = PixelSize {
            width: 1280,
            height: 720,
        };
        let mut exact = DisplayControlAdapter::new(initial);
        exact.set_negotiated(Some(u64::MAX));
        exact.observe_viewport(viewport(8192, 2048));
        assert_eq!(
            exact.take_resize_request(),
            Some(PixelSize {
                width: 8192,
                height: 2048,
            })
        );

        let mut over_budget = DisplayControlAdapter::new(initial);
        over_budget.set_negotiated(Some(u64::MAX));
        over_budget.observe_viewport(viewport(8192, 2049));
        assert_eq!(over_budget.take_resize_request(), None);
        assert_eq!(over_budget.confirmed_size(), initial);
    }

    #[test]
    fn display_server_area_accepts_a_layout_at_the_exact_limit() {
        let mut adapter = DisplayControlAdapter::new(PixelSize {
            width: 800,
            height: 600,
        });
        adapter.set_negotiated(Some(1600 * 900));

        adapter.observe_viewport(viewport(1600, 900));

        assert_eq!(
            adapter.take_resize_request(),
            Some(PixelSize {
                width: 1600,
                height: 900,
            })
        );
    }

    #[test]
    fn display_over_server_area_is_rejected_without_blocking_a_later_valid_viewport() {
        let initial = PixelSize {
            width: 800,
            height: 600,
        };
        let mut adapter = DisplayControlAdapter::new(initial);
        adapter.set_negotiated(Some(1600 * 900 - 1));

        adapter.observe_viewport(viewport(1600, 900));
        assert_eq!(adapter.take_resize_request(), None);
        assert_eq!(adapter.confirmed_size(), initial);

        adapter.observe_viewport(viewport(1280, 720));
        assert_eq!(
            adapter.take_resize_request(),
            Some(PixelSize {
                width: 1280,
                height: 720,
            })
        );
    }
}
