SamplerState smp_linear : register(s0);
Texture2D image : register(t0);
cbuffer Constants : register(b0) {
	float2 image_dim;
	float2 window_dim;
	float4 mouse; // float2 xy pos, uint buttons, uint unused
}

struct VSOut {
	float4 pos : SV_POSITION;
	float2 tex : TEXCOORD0;
};

VSOut blit_vs(uint i: SV_VERTEXID) {
	VSOut v[3] = {
		{ float4(-1,+1,0,1), float2(0,0) },
		{ float4(+3,+1,0,1), float2(2,0) },
		{ float4(-1,-3,0,1), float2(0,2) },
	};
	return v[i];
}

float4 blit_ps(VSOut v) : SV_TARGET {
	if (distance(v.pos.xy, mouse.xy) < 10) {
		return float4(1,1,1,1);
	}
	//return image.Load(int3(v.pos.xy, 0));
	float2 uv = v.pos.xy / window_dim;
	return image.SampleLevel(smp_linear, uv, 0);
}
