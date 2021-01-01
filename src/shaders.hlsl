struct Constants {
	float2 image_dim;
	float2 window_dim;
	float4 mouse; // float2 xy pos, uint buttons, uint unused
	float4 xfm_viewport_to_image_uv; // xy: scale, zw: offset
};

struct VSOut {
	float4 pos      : SV_POSITION;
	float4 clip_pos : TEXCOORD0;
	float2 tex      : TEXCOORD1;
};

SamplerState g_default_sampler : register(s0);
SamplerState g_linear_sampler : register(s1);
SamplerState g_point_sampler : register(s2);

Texture2D g_image : register(t0);
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

float4 blit_ps(VSOut v) : SV_TARGET {

	/*
	if (distance(v.pos.xy, g_constants.mouse.xy) < 10) {
		return float4(1,1,1,1);
	}
	*/

	float2 uv = viewport_to_image_uv(v.pos.xy);
	float4 image_color = g_image.SampleLevel(g_point_sampler, uv, 0);
	if (any(abs(uv-0.5) > 0.5) || g_constants.image_dim.x == 0) {
		image_color = background_color((uint2)(v.pos.xy));
	}

	return image_color;
}
