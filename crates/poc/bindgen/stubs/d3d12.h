/* Opaque stand-ins for the D3D12 types hwcontext_d3d12va.h names. The Rust side casts these
 * pointers to the windows crate's interfaces, so the bindings need no Windows SDK types. */
#pragma once

typedef struct ID3D12Device ID3D12Device;
typedef struct ID3D12VideoDevice ID3D12VideoDevice;
typedef struct ID3D12Fence ID3D12Fence;
typedef struct ID3D12Resource ID3D12Resource;
typedef void *HANDLE;

/* Plain C enums in the SDK headers: 32-bit values. */
typedef int D3D12_RESOURCE_FLAGS;
typedef int D3D12_HEAP_FLAGS;
typedef int DXGI_FORMAT;
