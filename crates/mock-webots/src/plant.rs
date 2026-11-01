//! Minimal rotary inertia used by the `mock-webots` executable.
//!
//! Integration is semi-implicit Euler (B6 doc):
//!
//! ```text
//! omega += (torque - b * omega) / J * dt
//! theta += omega * dt
//! ```

/// One rotational degree of freedom: inertia `j`, viscous damping `b`, angle
/// `theta` (rad) and angular velocity `omega` (rad/s).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RotaryJoint {
    pub j: f64,
    pub b: f64,
    pub theta: f64,
    pub omega: f64,
}

impl RotaryJoint {
    pub fn new(j: f64, b: f64) -> Self {
        Self {
            j,
            b,
            theta: 0.0,
            omega: 0.0,
        }
    }

    /// Advance one step. Invalid inertia or time steps are ignored so a bad model
    /// cannot poison the reference plant with NaN.
    pub fn step(&mut self, torque: f64, dt: f64) {
        if !self.j.is_finite()
            || self.j <= 0.0
            || !torque.is_finite()
            || !dt.is_finite()
            || dt <= 0.0
        {
            return;
        }
        self.omega += (torque - self.b * self.omega) / self.j * dt;
        self.theta += self.omega * dt;
    }
}

impl Default for RotaryJoint {
    fn default() -> Self {
        Self::new(0.01, 0.001)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_torque_uses_semi_implicit_euler() {
        let mut joint = RotaryJoint::new(2.0, 0.0);
        let dt = 0.1;
        joint.step(4.0, dt); // omega = 0.2, theta = 0.02
        assert!((joint.omega - 0.2).abs() < 1e-12);
        assert!((joint.theta - 0.02).abs() < 1e-12);
        joint.step(4.0, dt); // omega = 0.4, theta = 0.02 + 0.04
        assert!((joint.omega - 0.4).abs() < 1e-12);
        assert!((joint.theta - 0.06).abs() < 1e-12);
    }

    #[test]
    fn damping_reduces_velocity() {
        let mut joint = RotaryJoint::new(1.0, 2.0);
        joint.omega = 1.0;
        joint.step(0.0, 0.1);
        assert!(joint.omega < 1.0);
        assert!(joint.omega > 0.0);
    }

    #[test]
    fn invalid_inputs_are_ignored() {
        let mut joint = RotaryJoint::new(1.0, 0.0);
        joint.step(f64::NAN, 0.1);
        assert_eq!(joint, RotaryJoint::new(1.0, 0.0));

        let mut joint = RotaryJoint::new(0.0, 0.0);
        joint.step(1.0, 0.1);
        assert_eq!(joint.theta, 0.0);
        assert_eq!(joint.omega, 0.0);

        let mut joint = RotaryJoint::new(1.0, 0.0);
        joint.step(1.0, 0.0);
        assert_eq!(joint.theta, 0.0);
        assert_eq!(joint.omega, 0.0);
    }
}
