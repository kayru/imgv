struct Constants {
	float2 image_dim;
	float2 window_dim;
	float2 mouse_pos;
	uint mouse_buttons;
	uint show_transparency;
	float4 xfm_viewport_to_image_uv; // xy: scale, zw: offset
	uint image_type; // 0 = 2d/2darray, 1 = 3d
	uint current_slice;
	uint current_mip;
	uint slice_count;
	uint premultiplied_alpha;
};

struct VSOut {
	float4 pos      : SV_POSITION;
	float4 clip_pos : TEXCOORD0;
	float2 tex      : TEXCOORD1;
};

SamplerState g_default_sampler : register(s0);
SamplerState g_linear_sampler : register(s1);
SamplerState g_point_sampler : register(s2);

Texture2DArray g_image_array : register(t0);
Texture3D g_image_volume : register(t1);
cbuffer ConstantsCB : register(b0) { Constants g_constants; }

VSOut blit_vs(uint i: SV_VERTEXID) {
	VSOut v[3] = {
		{ float4(-1,+1,0,1), float4(-1,+1,0,1), float2(0,0) },
		{ float4(+3,+1,0,1), float4(+3,+1,0,1), float2(2,0) },
		{ float4(-1,-3,0,1), float4(-1,-3,0,1), float2(0,2) },
	};
	return v[i];
}

float4 background_color(uint2 pixel_pos) {
	pixel_pos /= 8;
	float c = 0.08;
	return ((pixel_pos.x + pixel_pos.y) & 1)
	? float4((float3)0.65 + c, 1.0)
	: float4((float3)0.65 - c, 1.0);
}

float2 viewport_to_image_uv(float2 viewport_pos) {
	float2 scale  = g_constants.xfm_viewport_to_image_uv.xy;
	float2 offset = g_constants.xfm_viewport_to_image_uv.zw;
	return viewport_pos * scale + offset;
}

float4 sample_image(float2 uv) {
	if (g_constants.image_type == 1) {
		float w = (g_constants.current_slice + 0.5) / (float)g_constants.slice_count;
		return g_image_volume.SampleLevel(g_point_sampler, float3(uv, w), g_constants.current_mip);
	}
	return g_image_array.SampleLevel(g_point_sampler, float3(uv, g_constants.current_slice), g_constants.current_mip);
}

float4 blit_ps(VSOut v) : SV_TARGET {
	float2 uv = viewport_to_image_uv(v.pos.xy);
	float4 bg = background_color((uint2)(v.pos.xy));
	if (any(abs(uv-0.5) > 0.5) || g_constants.image_dim.x == 0) {
		return bg;
	}
	float4 c = sample_image(uv);
	if (!g_constants.show_transparency) {
		return float4(c.rgb, 1.0);
	}
	if (g_constants.premultiplied_alpha) {
		return float4(c.rgb + bg.rgb * (1.0 - c.a), 1.0);
	}
	return float4(c.rgb * c.a + bg.rgb * (1.0 - c.a), 1.0);
}
