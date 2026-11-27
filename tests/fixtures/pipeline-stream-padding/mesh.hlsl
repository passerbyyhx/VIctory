struct Vertex {
    float4 position : SV_Position;
};

[outputtopology("triangle")]
[numthreads(1, 1, 1)]
void main(out vertices Vertex vertices[3], out indices uint3 triangles[1]) {
    SetMeshOutputCounts(3, 1);
    vertices[0].position = float4(-0.5, -0.5, 0.5, 1);
    vertices[1].position = float4(-0.5,  0.5, 0.5, 1);
    vertices[2].position = float4( 0.5,  0.5, 0.5, 1);
    triangles[0] = uint3(0, 1, 2);
}
