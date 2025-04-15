// This package contains benchmarks for the Eigenbundler
// See benches/eigenbundler.rs for the benchmark implementation

// Re-export types that might be useful for other benchmarks
pub mod utils {
    use rand::{rngs::SmallRng, Rng, SeedableRng};
    
    /// Generate random data of specified size
    pub fn generate_random_data(size_bytes: usize, seed: u64) -> Vec<u8> {
        let mut rng = SmallRng::seed_from_u64(seed);
        let mut data = vec![0u8; size_bytes];
        rng.fill(&mut data[..]);
        data
    }
    
    /// Generate highly compressible data (repeated patterns)
    pub fn generate_compressible_data(size_bytes: usize, seed: u64) -> Vec<u8> {
        let mut rng = SmallRng::seed_from_u64(seed);
        
        // Create a pattern to repeat
        let pattern_size = 64;
        let mut pattern = vec![0u8; pattern_size];
        rng.fill(&mut pattern[..]);
        
        let mut data = Vec::with_capacity(size_bytes);
        while data.len() < size_bytes {
            data.extend_from_slice(&pattern);
        }
        data.truncate(size_bytes);
        
        data
    }
}

pub fn add(left: u64, right: u64) -> u64 {
    left + right
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        let result = add(2, 2);
        assert_eq!(result, 4);
    }
}
