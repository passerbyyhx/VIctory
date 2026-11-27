struct VertexOutput {
    float4 position : SV_Position;
    uint rate : SV_ShadingRate;
};

VertexOutput vs_main(uint id : SV_VertexID) {
    VertexOutput v;
    v.position = float4(id == 1 ? 3 : -1, id == 2 ? -3 : 1, 0.5, 1);
    v.rate = 4; // D3D12_SHADING_RATE_2X1
    return v;
}

float4 ps_main(float4 position : SV_Position) : SV_Target {
    return float4(position.xy, 0.0, 1.0);
}
