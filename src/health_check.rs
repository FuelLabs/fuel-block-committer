pub type HealthChecker = Box<dyn HealthCheck + Send + Sync>;
pub trait HealthCheck {
    fn healthy(&self) -> bool;
}
