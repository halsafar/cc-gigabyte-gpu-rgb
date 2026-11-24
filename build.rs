// SPDX-License-Identifier: GPL-3.0-or-later
// Based on the CoolerControl device-service template (GPL-3.0-or-later).

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto");
    tonic_prost_build::configure()
        .build_server(true)
        // The client is only used by the integration tests, but generating it here
        // keeps the build script simple.
        .build_client(true)
        // needed for older protoc packages:
        .protoc_arg("--experimental_allow_proto3_optional")
        .compile_protos(
            &[
                "proto/coolercontrol/models/v1/device.proto",
                "proto/coolercontrol/device_service/v1/device_service.proto",
            ],
            &["proto"],
        )?;
    Ok(())
}
