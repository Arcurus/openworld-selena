//! Tick-Based Time System
//! 
//! Implements tick-based time tracking similar to OpenLife.
//! Time is measured in ticks, where X ticks = 1 game year (1 real minute).

/// Number of ticks per real second. This is the base tick rate.
pub const TICKS_PER_SECOND: f64 = 60.0;

/// Number of ticks per real minute (1 game year)
pub const TICKS_PER_YEAR: f64 = TICKS_PER_SECOND * 60.0;

/// Number of ticks per real hour (60 game years)
pub const TICKS_PER_HOUR: f64 = TICKS_PER_YEAR * 60.0;

/// Number of ticks per game day (assuming 365 days per year)
pub const TICKS_PER_DAY: f64 = TICKS_PER_YEAR / 365.0;

/// Number of ticks per game hour (assuming 24 hours per day)
pub const TICKS_PER_HOUR_GAME: f64 = TICKS_PER_DAY / 24.0;

/// Tick-based time representation
#[derive(Debug, Clone, Copy)]
pub struct TickTime {
    /// Total ticks elapsed since world creation
    pub ticks: f64,
    /// Last real time update (for delta calculation)
    pub last_real_time: Option<chrono::DateTime<chrono::Utc>>,
}

impl TickTime {
    /// Create new tick time starting at 0
    pub fn new() -> Self {
        Self {
            ticks: 0.0,
            last_real_time: Some(chrono::Utc::now()),
        }
    }
    
    /// Create tick time from a specific tick value
    pub fn from_ticks(ticks: f64) -> Self {
        Self {
            ticks,
            last_real_time: Some(chrono::Utc::now()),
        }
    }
    
    /// Advance time based on real elapsed time
    /// Called periodically to update ticks based on real time
    pub fn update_from_real_time(&mut self) {
        let now = chrono::Utc::now();
        
        if let Some(last_time) = self.last_real_time {
            let elapsed_seconds = (now - last_time).num_seconds() as f64;
            let elapsed_fractional = (now - last_time).num_milliseconds() as f64 / 1000.0 - elapsed_seconds as f64;
            let total_elapsed = elapsed_seconds + elapsed_fractional;
            
            if total_elapsed > 0.0 {
                self.ticks += total_elapsed * TICKS_PER_SECOND;
            }
        }
        
        self.last_real_time = Some(now);
    }
    
    /// Get current game years (as float for fractional years)
    pub fn years(&self) -> f64 {
        self.ticks / TICKS_PER_YEAR
    }
    
    /// Get current game days (assuming 365 days per year)
    pub fn days(&self) -> f64 {
        (self.ticks / TICKS_PER_DAY) % 365.0
    }
    
    /// Get current game hours (assuming 24 hours per day)
    pub fn hours(&self) -> f64 {
        (self.ticks / TICKS_PER_HOUR_GAME) % 24.0
    }
    
    /// Format as detailed time string
    pub fn formatted(&self) -> String {
        let years = self.years() as u32;
        let days = self.days() as u32;
        let hours = self.hours() as u32;
        
        format!("Year {}, Day {}, Hour {}", years, days, hours)
    }
}

impl Default for TickTime {
    fn default() -> Self {
        Self::new()
    }
}
