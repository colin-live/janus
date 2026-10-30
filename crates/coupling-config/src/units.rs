//! Unit parsing and SI conversion for vendor/model parameter files.
//!
//! Values handled by the coupling protocol are always SI. This module is the
//! single place where human-readable vendor units are converted, e.g.
//! `units::parse("3000 rpm") == 314.159...` (rad/s) and
//! `units::parse("25 degC") == 298.15` (K).
//!
//! Only the units listed in `docs/blocks/B3-config.md` plus their common
//! spellings are accepted. Parsing is deliberately strict about dimensions:
//! `parse_as("10 mm", Dim::Angle)` is an error.

use std::fmt;

/// Physical dimension of a quantity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dim {
    Angle,
    AngularVelocity,
    Length,
    LinearVelocity,
    Mass,
    Force,
    Torque,
    Current,
    Voltage,
    Resistance,
    Inductance,
    Temperature,
    Power,
    Time,
    Dimensionless,
}

impl Dim {
    /// Canonical SI spelling used in manifests.
    pub fn canonical_unit(self) -> &'static str {
        match self {
            Self::Angle => "rad",
            Self::AngularVelocity => "rad/s",
            Self::Length => "m",
            Self::LinearVelocity => "m/s",
            Self::Mass => "kg",
            Self::Force => "N",
            Self::Torque => "N*m",
            Self::Current => "A",
            Self::Voltage => "V",
            Self::Resistance => "ohm",
            Self::Inductance => "H",
            Self::Temperature => "K",
            Self::Power => "W",
            Self::Time => "s",
            Self::Dimensionless => "1",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnitError {
    Empty,
    MissingNumber(String),
    BadNumber(String),
    UnknownUnit(String),
    DimensionMismatch { expected: Dim, got: Dim },
    NonFinite,
}

impl fmt::Display for UnitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "empty quantity"),
            Self::MissingNumber(s) => write!(f, "quantity {s:?} has no numeric value"),
            Self::BadNumber(s) => write!(f, "invalid number {s:?}"),
            Self::UnknownUnit(u) => write!(f, "unknown unit {u:?}"),
            Self::DimensionMismatch { expected, got } => {
                write!(
                    f,
                    "unit dimension mismatch: expected {expected:?}, got {got:?}"
                )
            }
            Self::NonFinite => write!(f, "quantity is not finite"),
        }
    }
}

impl std::error::Error for UnitError {}

struct UnitDef {
    aliases: &'static [&'static str],
    dim: Dim,
    /// SI = value * factor + offset
    factor: f64,
    offset: f64,
}

const fn simple(aliases: &'static [&'static str], dim: Dim, factor: f64) -> UnitDef {
    UnitDef {
        aliases,
        dim,
        factor,
        offset: 0.0,
    }
}

const UNITS: &[UnitDef] = &[
    // angle
    simple(&["rad", "radian", "radians"], Dim::Angle, 1.0),
    simple(
        &["deg", "degree", "degrees"],
        Dim::Angle,
        std::f64::consts::PI / 180.0,
    ),
    // angular velocity
    simple(
        &[
            "rad/s",
            "rads",
            "rad/sec",
            "rad/second",
            "radians/s",
            "radians/sec",
        ],
        Dim::AngularVelocity,
        1.0,
    ),
    simple(
        &["deg/s", "deg/sec", "degrees/s", "degrees/sec", "dps"],
        Dim::AngularVelocity,
        std::f64::consts::PI / 180.0,
    ),
    simple(
        &["rpm", "r/min", "rev/min", "revolutions/min"],
        Dim::AngularVelocity,
        2.0 * std::f64::consts::PI / 60.0,
    ),
    // length
    simple(
        &["m", "meter", "meters", "metre", "metres"],
        Dim::Length,
        1.0,
    ),
    simple(
        &[
            "mm",
            "millimeter",
            "millimeters",
            "millimetre",
            "millimetres",
        ],
        Dim::Length,
        1e-3,
    ),
    simple(
        &[
            "cm",
            "centimeter",
            "centimeters",
            "centimetre",
            "centimetres",
        ],
        Dim::Length,
        1e-2,
    ),
    simple(
        &["km", "kilometer", "kilometers", "kilometre", "kilometres"],
        Dim::Length,
        1e3,
    ),
    simple(
        &[
            "um",
            "micrometer",
            "micrometers",
            "micrometre",
            "micrometres",
        ],
        Dim::Length,
        1e-6,
    ),
    // linear velocity
    simple(
        &["m/s", "mps", "meter/s", "meters/s", "metre/s", "metres/s"],
        Dim::LinearVelocity,
        1.0,
    ),
    simple(
        &["mm/s", "millimeter/s", "millimeters/s"],
        Dim::LinearVelocity,
        1e-3,
    ),
    // mass
    simple(&["kg", "kilogram", "kilograms"], Dim::Mass, 1.0),
    simple(&["g", "gram", "grams"], Dim::Mass, 1e-3),
    simple(&["mg", "milligram", "milligrams"], Dim::Mass, 1e-6),
    // force
    simple(&["n", "newton", "newtons"], Dim::Force, 1.0),
    simple(&["mn", "millinewton", "millinewtons"], Dim::Force, 1e-3),
    simple(&["kn", "kilonewton", "kilonewtons"], Dim::Force, 1e3),
    // torque
    simple(
        &[
            "n*m",
            "nm",
            "n.m",
            "newton*meter",
            "newton*meters",
            "newtonmeter",
        ],
        Dim::Torque,
        1.0,
    ),
    simple(
        &["mn*m", "mnm", "mn.m", "millinewton*meter"],
        Dim::Torque,
        1e-3,
    ),
    simple(&["kn*m", "knm", "kilonewton*meter"], Dim::Torque, 1e3),
    // current
    simple(
        &["a", "amp", "amps", "ampere", "amperes"],
        Dim::Current,
        1.0,
    ),
    simple(
        &["ma", "milliamp", "milliamps", "milliampere", "milliamperes"],
        Dim::Current,
        1e-3,
    ),
    // voltage
    simple(&["v", "volt", "volts"], Dim::Voltage, 1.0),
    simple(&["mv", "millivolt", "millivolts"], Dim::Voltage, 1e-3),
    simple(&["kv", "kilovolt", "kilovolts"], Dim::Voltage, 1e3),
    // resistance
    simple(&["ohm", "ohms"], Dim::Resistance, 1.0),
    simple(&["kohm", "kiloohm", "kiloohms"], Dim::Resistance, 1e3),
    simple(&["mohm", "milliohm", "milliohms"], Dim::Resistance, 1e-3),
    // inductance
    simple(&["h", "henry", "henries", "henrys"], Dim::Inductance, 1.0),
    simple(&["mh", "millihenry", "millihenries"], Dim::Inductance, 1e-3),
    simple(&["uh", "microhenry", "microhenries"], Dim::Inductance, 1e-6),
    // temperature (affine)
    simple(&["k", "kelvin", "kelvins"], Dim::Temperature, 1.0),
    UnitDef {
        aliases: &["degc", "c", "celsius", "degreecelsius", "degreescelsius"],
        dim: Dim::Temperature,
        factor: 1.0,
        offset: 273.15,
    },
    UnitDef {
        aliases: &[
            "degf",
            "f",
            "fahrenheit",
            "degreefahrenheit",
            "degreesfahrenheit",
        ],
        dim: Dim::Temperature,
        factor: 5.0 / 9.0,
        offset: 459.67 * 5.0 / 9.0,
    },
    // power
    simple(&["w", "watt", "watts"], Dim::Power, 1.0),
    simple(&["kw", "kilowatt", "kilowatts"], Dim::Power, 1e3),
    simple(&["mw", "megawatt", "megawatts"], Dim::Power, 1e6),
    // time
    simple(&["s", "sec", "secs", "second", "seconds"], Dim::Time, 1.0),
    simple(
        &["ms", "msec", "millisecond", "milliseconds"],
        Dim::Time,
        1e-3,
    ),
    simple(
        &["us", "usec", "microsecond", "microseconds"],
        Dim::Time,
        1e-6,
    ),
    simple(
        &["ns", "nsec", "nanosecond", "nanoseconds"],
        Dim::Time,
        1e-9,
    ),
    simple(&["min", "minute", "minutes"], Dim::Time, 60.0),
    // dimensionless
    simple(
        &["1", "", "-", "dimensionless", "ratio"],
        Dim::Dimensionless,
        1.0,
    ),
];

/// Normalise unit spelling: lower-case, strip whitespace, unify micro/ohm/degree
/// characters and torque separators.
fn normalize_unit(unit: &str) -> String {
    let mut out = String::with_capacity(unit.len());
    for ch in unit.trim().chars() {
        if ch.is_whitespace() {
            continue;
        }
        match ch {
            '°' => out.push_str("deg"),
            'µ' | 'μ' => out.push('u'),
            'Ω' | 'ω' => out.push_str("ohm"),
            '·' | '⋅' | '∙' => out.push('*'),
            _ => out.extend(ch.to_lowercase()),
        }
    }
    out
}

fn find_unit(normalized: &str) -> Option<&'static UnitDef> {
    UNITS.iter().find(|def| def.aliases.contains(&normalized))
}

/// Split `"3000 rpm"` into `(3000.0, "rpm")`.
fn split_quantity(input: &str) -> Result<(f64, &str), UnitError> {
    let s = input.trim();
    if s.is_empty() {
        return Err(UnitError::Empty);
    }
    let bytes = s.as_bytes();
    let mut i = 0;
    if bytes[0] == b'+' || bytes[0] == b'-' {
        i += 1;
    }
    let mut digits = 0usize;
    while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
        if bytes[i].is_ascii_digit() {
            digits += 1;
        }
        i += 1;
    }
    if digits == 0 {
        return Err(UnitError::MissingNumber(s.to_string()));
    }
    // Optional exponent. Only consume `e`/`E` when it is followed by a valid
    // exponent, so units such as "eV" would not be swallowed accidentally.
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        let save = i;
        i += 1;
        if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
            i += 1;
        }
        let exp_start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == exp_start {
            i = save;
        }
    }
    let number = &s[..i];
    let value: f64 = number
        .parse()
        .map_err(|_| UnitError::BadNumber(number.to_string()))?;
    if !value.is_finite() {
        return Err(UnitError::NonFinite);
    }
    Ok((value, s[i..].trim()))
}

/// Parse a quantity with a unit and return its SI value.
///
/// The dimension is taken from the unit, so `"3000 rpm"` yields rad/s and
/// `"25 degC"` yields kelvin.
pub fn parse(s: &str) -> Result<f64, UnitError> {
    let (value, unit) = split_quantity(s)?;
    let normalized = normalize_unit(unit);
    let def = find_unit(&normalized).ok_or_else(|| UnitError::UnknownUnit(unit.to_string()))?;
    let si = value * def.factor + def.offset;
    if si.is_finite() {
        Ok(si)
    } else {
        Err(UnitError::NonFinite)
    }
}

/// Parse a quantity and require a specific dimension.
pub fn parse_as(s: &str, dim: Dim) -> Result<f64, UnitError> {
    let (value, unit) = split_quantity(s)?;
    let normalized = normalize_unit(unit);
    let def = find_unit(&normalized).ok_or_else(|| UnitError::UnknownUnit(unit.to_string()))?;
    if def.dim != dim {
        return Err(UnitError::DimensionMismatch {
            expected: dim,
            got: def.dim,
        });
    }
    let si = value * def.factor + def.offset;
    if si.is_finite() {
        Ok(si)
    } else {
        Err(UnitError::NonFinite)
    }
}

/// Dimension of `unit`, or an error if the unit is unknown.
pub fn dim_of(unit: &str) -> Result<Dim, UnitError> {
    let normalized = normalize_unit(unit);
    find_unit(&normalized)
        .map(|def| def.dim)
        .ok_or_else(|| UnitError::UnknownUnit(unit.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }

    #[test]
    fn rpm_and_degrees() {
        close(
            parse("3000 rpm").unwrap(),
            3000.0 * std::f64::consts::PI / 30.0,
        );
        close(parse("180 deg").unwrap(), std::f64::consts::PI);
    }

    #[test]
    fn temperature_is_affine() {
        close(parse("25 degC").unwrap(), 298.15);
        close(parse("298.15 K").unwrap(), 298.15);
        close(parse("-40 degF").unwrap(), 233.15);
    }

    #[test]
    fn documented_unit_table_is_covered() {
        let cases: &[(&str, Dim, f64)] = &[
            ("3000 rpm", Dim::AngularVelocity, 314.159_265_358_979_3),
            ("1 rad/s", Dim::AngularVelocity, 1.0),
            ("180 deg", Dim::Angle, std::f64::consts::PI),
            ("1 rad", Dim::Angle, 1.0),
            ("10 mm", Dim::Length, 0.01),
            ("1 cm", Dim::Length, 0.01),
            ("1 m", Dim::Length, 1.0),
            ("1 kg", Dim::Mass, 1.0),
            ("1000 g", Dim::Mass, 1.0),
            ("1 N", Dim::Force, 1.0),
            ("1 N*m", Dim::Torque, 1.0),
            ("1 mN*m", Dim::Torque, 1e-3),
            ("1 A", Dim::Current, 1.0),
            ("1 V", Dim::Voltage, 1.0),
            ("1 ohm", Dim::Resistance, 1.0),
            ("1 mH", Dim::Inductance, 1e-3),
            ("1 H", Dim::Inductance, 1.0),
            ("25 degC", Dim::Temperature, 298.15),
            ("32 degF", Dim::Temperature, 273.15),
            ("273.15 K", Dim::Temperature, 273.15),
            ("1 W", Dim::Power, 1.0),
            ("1 kW", Dim::Power, 1000.0),
            ("1 ms", Dim::Time, 1e-3),
            ("1 s", Dim::Time, 1.0),
            ("1 us", Dim::Time, 1e-6),
        ];
        for (text, dim, expected) in cases {
            let value = parse_as(text, *dim).unwrap_or_else(|e| panic!("{text}: {e}"));
            let tolerance = expected.abs().max(1.0) * 1e-12;
            assert!(
                (value - expected).abs() <= tolerance,
                "{text}: {value} != {expected}"
            );
        }
    }

    #[test]
    fn metric_prefixes() {
        close(parse("10 mm").unwrap(), 0.01);
        close(parse("2 mH").unwrap(), 0.002);
        close(parse("500 ms").unwrap(), 0.5);
        close(parse("1.5 kW").unwrap(), 1500.0);
    }

    #[test]
    fn dimension_mismatch_is_rejected() {
        assert_eq!(
            parse_as("10 mm", Dim::Angle),
            Err(UnitError::DimensionMismatch {
                expected: Dim::Angle,
                got: Dim::Length,
            })
        );
        assert!(parse_as("3000 rpm", Dim::AngularVelocity).is_ok());
        assert!(parse_as("3000 rpm", Dim::Angle).is_err());
    }

    #[test]
    fn bad_inputs_are_rejected() {
        assert_eq!(parse(""), Err(UnitError::Empty));
        assert!(matches!(parse("rpm"), Err(UnitError::MissingNumber(_))));
        assert!(matches!(parse("1 furlong"), Err(UnitError::UnknownUnit(_))));
        assert!(matches!(parse("1e999 m"), Err(UnitError::NonFinite)));
    }
}
