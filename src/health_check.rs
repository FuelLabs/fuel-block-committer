pub trait HealthCheck {
    fn healthy(&self) -> bool;
}
