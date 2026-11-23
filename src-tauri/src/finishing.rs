use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransformKey {
    pub frame: u32,
    pub scale: f64,
    pub x: f64,
    pub y: f64,
    pub opacity: f64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Finishing {
    pub speed: f64,
    pub brightness: f64,
    pub contrast: f64,
    pub saturation: f64,
    pub balance: [f64; 3],
    pub cube: Option<String>,
    pub transforms: Vec<TransformKey>,
    pub denoise: bool,
    pub level: bool,
    pub role: String,
    pub auto_duck: bool,
    pub gain: f64,
}
impl Default for Finishing {
    fn default() -> Self {
        Self {
            speed: 1.0,
            brightness: 0.0,
            contrast: 1.0,
            saturation: 1.0,
            balance: [0.0; 3],
            cube: None,
            transforms: vec![],
            denoise: false,
            level: false,
            role: "normal".into(),
            auto_duck: false,
            gain: 1.0,
        }
    }
}
impl Finishing {
    pub fn validate(&self, kind: &str, max: u32) -> Result<(), String> {
        let valid = |n: f64, lo: f64, hi: f64| n.is_finite() && n >= lo && n <= hi;
        if !valid(self.speed, 0.25, 4.0)
            || !valid(self.brightness, -1.0, 1.0)
            || !valid(self.contrast, 0.0, 2.0)
            || !valid(self.saturation, 0.0, 3.0)
            || !valid(self.gain, 0.0, 2.0)
            || self.balance.iter().any(|n| !valid(*n, -1.0, 1.0))
            || !["normal", "voice", "music"].contains(&self.role.as_str())
            || self.transforms.len() > 64
            || (self.auto_duck && self.role != "music")
        {
            return Err("Invalid finishing settings".into());
        }
        let d = Self::default();
        if (kind != "video"
            && (self.brightness != d.brightness
                || self.contrast != d.contrast
                || self.saturation != d.saturation
                || self.balance != d.balance
                || self.cube.is_some()
                || !self.transforms.is_empty()))
            || (kind != "audio"
                && (self.denoise
                    || self.level
                    || self.auto_duck
                    || self.role != d.role
                    || self.gain != 1.0))
            || (kind == "caption" && self.speed != 1.0)
        {
            return Err("Finishing settings incompatible with track".into());
        }
        for (i, k) in self.transforms.iter().enumerate() {
            if k.frame > max
                || (i > 0 && k.frame <= self.transforms[i - 1].frame)
                || !valid(k.scale, 0.1, 4.0)
                || !valid(k.x, -1.0, 1.0)
                || !valid(k.y, -1.0, 1.0)
                || !valid(k.opacity, 0.0, 1.0)
            {
                return Err("Invalid transform keyframes".into());
            }
        }
        if let Some(cube) = &self.cube {
            validate_cube(cube)?;
        }
        Ok(())
    }
}

pub fn validate_cube(text: &str) -> Result<u32, String> {
    if text.len() > 262144 || text.contains('\0') {
        return Err("LUT exceeds 256 KB or contains NUL".into());
    }
    let mut size = None;
    let mut rows = 0;
    let mut domain_min = false;
    let mut domain_max = false;
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let fields: Vec<_> = line.split_whitespace().collect();
        match fields[0] {
            "TITLE" if rows == 0 && line[5..].trim().starts_with('"') && line.ends_with('"') => {}
            "LUT_3D_SIZE" if fields.len() == 2 && size.is_none() && rows == 0 => {
                let n = fields[1].parse::<u32>().map_err(|_| "Invalid LUT size")?;
                if !(2..=33).contains(&n) {
                    return Err("LUT size must be 2–33".into());
                }
                size = Some(n);
            }
            "DOMAIN_MIN" | "DOMAIN_MAX" if fields.len() == 4 && rows == 0 => {
                let minimum = fields[0] == "DOMAIN_MIN";
                let seen = if minimum {
                    &mut domain_min
                } else {
                    &mut domain_max
                };
                if *seen {
                    return Err("Duplicate LUT domain".into());
                }
                *seen = true;
                for f in &fields[1..] {
                    if f.parse::<f64>().map_err(|_| "Invalid LUT domain")?
                        != if minimum { 0.0 } else { 1.0 }
                    {
                        return Err("Only RGB domain 0..1 LUTs are supported".into());
                    }
                }
            }
            _ if fields.len() == 3 && size.is_some() => {
                for f in fields {
                    let n = f.parse::<f64>().map_err(|_| "Invalid LUT row")?;
                    if !n.is_finite() || !(0.0..=1.0).contains(&n) {
                        return Err("LUT values must be finite RGB 0..1".into());
                    }
                }
                rows += 1;
            }
            _ => return Err("Unsupported or malformed .cube directive".into()),
        }
    }
    let size = size.ok_or("Missing LUT_3D_SIZE")?;
    if rows != size.pow(3) {
        return Err("LUT row count does not match size cubed".into());
    }
    Ok(size)
}

pub fn preset(name: &str) -> Result<String, String> {
    if !["identity", "warm", "cool"].contains(&name) {
        return Err("Unknown LUT preset".into());
    }
    let mut text = format!("# StudioCut original LUT; CC0-1.0\nTITLE \"StudioCut {name}\"\nLUT_3D_SIZE 17\nDOMAIN_MIN 0 0 0\nDOMAIN_MAX 1 1 1\n");
    for b in 0..17 {
        for g in 0..17 {
            for r in 0..17 {
                let (r, g, b) = (r as f64 / 16.0, g as f64 / 16.0, b as f64 / 16.0);
                let rgb = match name {
                    "warm" => [r * 1.04 + g * 0.015, g, b * 0.92],
                    "cool" => [r * 0.94, g, b * 1.04 + g * 0.01],
                    _ => [r, g, b],
                };
                text.push_str(&format!(
                    "{:.6} {:.6} {:.6}\n",
                    rgb[0].clamp(0.0, 1.0),
                    rgb[1].clamp(0.0, 1.0),
                    rgb[2].clamp(0.0, 1.0)
                ));
            }
        }
    }
    Ok(text)
}
