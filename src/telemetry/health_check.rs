pub type HealthChecker = Box<dyn HealthCheck>;
pub trait HealthCheck: Send + Sync {
    fn healthy(&self) -> bool;
}
