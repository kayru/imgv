use cgmath::{assert_ulps_eq, prelude::*};

#[allow(non_camel_case_types)]
pub type float2 = cgmath::Vector2<f32>;

#[allow(non_camel_case_types)]
pub type float3 = cgmath::Vector3<f32>;

#[allow(non_camel_case_types)]
pub type float4 = cgmath::Vector4<f32>;

pub const FLOAT2_ONE: float2 = float2::new(1.0, 1.0);
pub const FLOAT3_ONE: float3 = float3::new(1.0, 1.0, 1.0);
pub const FLOAT4_ONE: float4 = float4::new(1.0, 1.0, 1.0, 1.0);

pub const FLOAT2_ZERO: float2 = float2::new(0.0, 0.0);
pub const FLOAT3_ZERO: float3 = float3::new(0.0, 0.0, 0.0);
pub const FLOAT4_ZERO: float4 = float4::new(0.0, 0.0, 0.0, 0.0);

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Box2D {
    pub min: float2,
    pub max: float2,
}

impl Box2D {
    pub fn center(&self) -> float2 {
        (self.min + self.max) * 0.5
    }
    pub fn width(&self) -> f32 {
        self.dim().x
    }
    pub fn height(&self) -> f32 {
        self.dim().y
    }
    pub fn dim(&self) -> float2 {
        self.max - self.min
    }
}

impl Default for Box2D {
    fn default() -> Self {
        Box2D { min: FLOAT2_ZERO, max: FLOAT2_ZERO }
    }
}
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Transform2D {
    pub scale: float2,
    pub offset: float2,
}

impl Transform2D {
    pub fn new_identity() -> Self {
        Transform2D {
            scale: FLOAT2_ONE,
            offset: FLOAT2_ZERO,
        }
    }
    pub fn new_scale(scale: float2) -> Self {
        Transform2D {
            scale,
            offset: FLOAT2_ZERO,
        }
    }
    pub fn new_translate(offset: float2) -> Self {
        Transform2D {
            scale: FLOAT2_ONE,
            offset,
        }
    }
    pub fn inplace_translate(&mut self, v: float2) {
        self.offset += v;
    }
    pub fn translate(&self, v: float2) -> Self {
        let mut res = *self;
        res.inplace_translate(v);
        res
    }
    pub fn transform_point(&self, v: float2) -> float2 {
        v.mul_element_wise(self.scale) + self.offset
    }
    pub fn transform_box(&self, r: Box2D) -> Box2D {
        Box2D {
            min: self.transform_point(r.min),
            max: self.transform_point(r.max),
        }
    }
    pub fn inverse(&self) -> Self {
        Self {
            scale: 1.0 / self.scale,
            offset: -self.offset.div_element_wise(self.scale),
        }
    }
    pub fn concatenate(&self, other: Self) -> Self {
        Self {
            scale: self.scale.mul_element_wise(other.scale),
            offset: self.offset.mul_element_wise(other.scale) + other.offset,
        }
    }
    pub fn inplace_concatenate(&mut self, other: Self) {
        *self = self.concatenate(other);
    }
    pub fn to_float4(&self) -> float4 {
        float4::new(self.scale.x, self.scale.y, self.offset.x, self.offset.y)
    }
}

impl Default for Transform2D {
    fn default() -> Self {
        Transform2D::new_identity()
    }
}

pub fn clamp(x: f32, min: f32, max: f32) -> f32 {
    x.max(min).min(max)
}

#[test]
fn test_transform() {
    {
        let t = Transform2D {
            scale: float2::new(2.0, 3.0),
            offset: float2::new(4.0, 5.0),
        };
        let pa = float2::new(123.0, 456.0);
        let pb = t.transform_point(pa);
        let pc = t.inverse().transform_point(pb);
        assert_ulps_eq!(pa, pc);
    }
    {
        let t = Transform2D {
            scale: float2::new(2.0, 3.0),
            offset: float2::new(4.0, 5.0),
        };
        let pa = float2::new(1.0, 1.0);
        let pb = t.transform_point(pa);
        assert_ulps_eq!(pb, float2::new(6.0, 8.0));
    }
    {
        let ta = Transform2D {
            scale: float2::new(2.0, 3.0),
            offset: float2::new(4.0, 5.0),
        };
        let tb = Transform2D {
            scale: float2::new(3.0, 4.0),
            offset: float2::new(5.0, 6.0),
        };
        let tc = ta.concatenate(tb);
        let pa = float2::new(1.0, 1.0);
        let pb = tc.transform_point(pa);
        assert_ulps_eq!(pb, float2::new(23.0, 38.0));
    }
    {
        let t = Transform2D {
            scale: float2::new(1.0, 1.0),
            offset: float2::new(4.0, 5.0),
        };
        let b = Box2D {
            min: float2::new(0.0, 0.0),
            max: float2::new(1.0, 1.0),
        };
        let b2 = t.transform_box(b);
        assert_ulps_eq!(b2.min, float2::new(4.0, 5.0));
        assert_ulps_eq!(b2.max, float2::new(5.0, 6.0));
    }
}

