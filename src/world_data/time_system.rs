//! Time System for Open World
//! 
//! Adds a sense of time passage to the world - days, time of day, seasons.
//! This makes the world feel more alive as entities can have routines,
//! resources can change over time, and events can be time-based.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Time of day periods
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeOfDay {
    Dawn,
    Morning,
    Afternoon,
    Evening,
    Night,
}

impl TimeOfDay {
    /// Get the hour range for this time of day (0-23)
    pub fn hour_range(&self) -> (u8, u8) {
        match self {
            TimeOfDay::Dawn => (5, 7),
            TimeOfDay::Morning => (8, 11),
            TimeOfDay::Afternoon => (12, 16),
            TimeOfDay::Evening => (17, 20),
            TimeOfDay::Night => (21, 4),
        }
    }
    
    /// Get a descriptive name
    pub fn name(&self) -> &'static str {
        match self {
            TimeOfDay::Dawn => "Dawn",
            TimeOfDay::Morning => "Morning",
            TimeOfDay::Afternoon => "Afternoon",
            TimeOfDay::Evening => "Evening",
            TimeOfDay::Night => "Night",
        }
    }
    
    /// Check if this is typically an "active" time for most entities
    pub fn is_active_time(&self) -> bool {
        matches!(self, TimeOfDay::Morning | TimeOfDay::Afternoon | TimeOfDay::Evening)
    }
    
    /// Get time of day from hour (0-23)
    pub fn from_hour(hour: u8) -> Self {
        match hour {
            5..=7 => TimeOfDay::Dawn,
            8..=11 => TimeOfDay::Morning,
            12..=16 => TimeOfDay::Afternoon,
            17..=20 => TimeOfDay::Evening,
            _ => TimeOfDay::Night,
        }
    }
}

/// Seasons
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Season {
    Spring,
    Summer,
    Autumn,
    Winter,
}

impl Season {
    pub fn name(&self) -> &'static str {
        match self {
            Season::Spring => "Spring",
            Season::Summer => "Summer",
            Season::Autumn => "Autumn",
            Season::Winter => "Winter",
        }
    }
    
    /// Get season from day of year (0-365)
    pub fn from_day(day: u16) -> Self {
        let day_in_year = day % 365;
        match day_in_year {
            0..=89 => Season::Spring,   // Days 0-89 (Spring starts day 0)
            90..=179 => Season::Summer,  // Days 90-179
            180..=269 => Season::Autumn, // Days 180-269
            _ => Season::Winter,         // Days 270-364
        }
    }
    
    /// Get a multiplier for certain activities based on season
    pub fn activity_modifier(&self, activity: &str) -> f64 {
        match (self, activity) {
            (Season::Spring, "farming") => 1.5,
            (Season::Spring, "travel") => 1.2,
            (Season::Summer, "farming") => 1.3,
            (Season::Summer, "exploration") => 1.4,
            (Season::Summer, "combat") => 1.1,
            (Season::Autumn, "harvest") => 2.0,
            (Season::Autumn, "travel") => 1.1,
            (Season::Winter, "farming") => 0.0,
            (Season::Winter, "combat") => 1.2,
            (Season::Winter, "travel") => 0.7,
            _ => 1.0,
        }
    }
}

/// World time tracking - tick-based like OpenLife
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldTime {
    /// Current day number (world started at day 0)
    pub day: u32,
    /// Current hour (0-23)
    pub hour: u8,
    /// Actions taken this day (resets at midnight)
    pub actions_today: u32,
    /// Last real time update (for tick-based time)
    pub last_real_time: Option<DateTime<Utc>>,
    /// Total years elapsed (for display purposes)
    pub total_years: f64,
}

impl WorldTime {
    pub fn new() -> Self {
        Self {
            day: 0,
            hour: 8, // Start at morning
            actions_today: 0,
            last_real_time: Some(Utc::now()),
            total_years: 0.0,
        }
    }
    
    /// Tick rate: 1 real second = ~6 game days, so 1 real minute = 1 game year
    /// This means: 60 real seconds = 365 game days = 1 game year
    const REAL_SECONDS_PER_GAME_YEAR: f64 = 60.0;
    const GAME_DAYS_PER_GAME_YEAR: f64 = 365.0;
    const GAME_HOURS_PER_DAY: u32 = 24;
    
    /// Update time based on real elapsed time since last update
    /// Call this when world is loaded or accessed to advance time
    pub fn update_from_real_time(&mut self) {
        let now = Utc::now();
        
        if let Some(last_time) = self.last_real_time {
            let elapsed_seconds = (now - last_time).num_seconds() as f64;
            
            if elapsed_seconds > 0.0 {
                // Calculate game years that have passed
                let game_years_elapsed = elapsed_seconds / Self::REAL_SECONDS_PER_GAME_YEAR;
                
                if game_years_elapsed >= 0.001 { // At least ~1 game day
                    self.total_years += game_years_elapsed;
                    
                    // Convert to game days and hours
                    let game_days_elapsed = game_years_elapsed * Self::GAME_DAYS_PER_GAME_YEAR;
                    let total_game_hours = (game_days_elapsed * Self::GAME_HOURS_PER_DAY as f64) as u64;
                    
                    // Add to current time
                    let current_total_hours = (self.day as u64 * 24) + (self.hour as u64);
                    let new_total_hours = current_total_hours + total_game_hours;
                    
                    self.day = (new_total_hours / 24) as u32;
                    self.hour = (new_total_hours % 24) as u8;
                    
                    // Reset actions counter if we crossed midnight
                    // (simplified - could be more accurate)
                    self.actions_today = 0;
                }
            }
        }
        
        self.last_real_time = Some(now);
    }
    
    /// Advance time by some number of hours (legacy method, for manual advancement)
    pub fn advance(&mut self, hours: u8) {
        let new_hour = self.hour + hours;
        if new_hour >= 24 {
            let days_to_add = new_hour / 24;
            self.day += days_to_add as u32;
            self.hour = new_hour % 24;
        } else {
            self.hour = new_hour;
        }
        self.actions_today += 1;
    }
    
    /// Get the current time of day
    pub fn time_of_day(&self) -> TimeOfDay {
        TimeOfDay::from_hour(self.hour)
    }
    
    /// Get the current season
    pub fn season(&self) -> Season {
        Season::from_day(self.day as u16)
    }
    
    /// Get a formatted time string
    pub fn formatted_time(&self) -> String {
        let year = self.total_years.floor();
        format!("Year {} - {} - Day {}", year as u32, self.time_of_day().name(), self.day + 1)
    }
    
    /// Get a detailed formatted string
    pub fn detailed_time(&self) -> String {
        let season = self.season();
        let year = self.total_years.floor();
        format!("Year {} ({}), {} - Day {} - Hour {}", 
            year as u32, season.name(), self.time_of_day().name(), self.day + 1, self.hour)
    }
    
    /// Check if it's nighttime
    pub fn is_night(&self) -> bool {
        self.time_of_day() == TimeOfDay::Night
    }
    
    /// Check if it's daytime
    pub fn is_daytime(&self) -> bool {
        matches!(self.time_of_day(), TimeOfDay::Dawn | TimeOfDay::Morning | TimeOfDay::Afternoon | TimeOfDay::Evening)
    }
}

impl Default for WorldTime {
    fn default() -> Self {
        Self::new()
    }
}

/// Entity time preferences
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityTimePreferences {
    /// Preferred active times (when this entity is most active)
    pub active_times: Vec<TimeOfDay>,
    /// Whether this entity is nocturnal
    pub nocturnal: bool,
    /// Seasonal activity modifier name (e.g., "farming", "combat")
    pub seasonal_activity: Option<String>,
}

impl EntityTimePreferences {
    pub fn new() -> Self {
        Self {
            active_times: vec![TimeOfDay::Morning, TimeOfDay::Afternoon, TimeOfDay::Evening],
            nocturnal: false,
            seasonal_activity: None,
        }
    }
    
    /// Check if this entity is active at the given time
    pub fn is_active_at(&self, time: &WorldTime) -> bool {
        let tod = time.time_of_day();
        
        // Nocturnal entities are active at night
        if self.nocturnal {
            return tod == TimeOfDay::Night || tod == TimeOfDay::Dawn;
        }
        
        // Otherwise check active times
        self.active_times.contains(&tod)
    }
    
    /// Get activity multiplier based on current time
    pub fn activity_multiplier(&self, time: &WorldTime) -> f64 {
        if self.is_active_at(time) {
            1.0
        } else {
            0.3 // Less active during off hours
        }
    }
}

impl Default for EntityTimePreferences {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_time_of_day_from_hour() {
        assert_eq!(TimeOfDay::from_hour(6), TimeOfDay::Dawn);
        assert_eq!(TimeOfDay::from_hour(10), TimeOfDay::Morning);
        assert_eq!(TimeOfDay::from_hour(14), TimeOfDay::Afternoon);
        assert_eq!(TimeOfDay::from_hour(18), TimeOfDay::Evening);
        assert_eq!(TimeOfDay::from_hour(22), TimeOfDay::Night);
    }
    
    #[test]
    fn test_season_from_day() {
        assert_eq!(Season::from_day(45), Season::Spring);
        assert_eq!(Season::from_day(120), Season::Summer);
        assert_eq!(Season::from_day(200), Season::Autumn);
        assert_eq!(Season::from_day(300), Season::Winter);
    }
    
    #[test]
    fn test_world_time_advance() {
        let mut time = WorldTime::new();
        assert_eq!(time.hour, 8);
        assert_eq!(time.day, 0);
        
        time.advance(3);
        assert_eq!(time.hour, 11);
        
        time.advance(5); // Should wrap to next day
        assert_eq!(time.hour, 16);
        assert_eq!(time.day, 0);
        
        time.advance(10); // Wrap to next day
        assert_eq!(time.hour, 2);
        assert_eq!(time.day, 1);
    }
    
    #[test]
    fn test_entity_time_preferences() {
        let prefs = EntityTimePreferences::new();
        let mut time = WorldTime::new();
        
        // During morning, should be active
        time.hour = 9;
        assert!(prefs.is_active_at(&time));
        
        // During night, should not be active (unless nocturnal)
        time.hour = 22;
        assert!(!prefs.is_active_at(&time));
    }
}
