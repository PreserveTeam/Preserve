fn main() {
    println!("cargo:rerun-if-env-changed=PRESERVE_APP_VERSION");

    #[cfg(windows)]
    {
        let mut resource = winres::WindowsResource::new();
        resource.set_icon("assets/preserve.ico");
        // Prerequisite installers (msiexec, VC++/.NET bootstrappers) require
        // an elevated token to run. Requesting elevation at launch, rather
        // than partway through an install, avoids ERROR_ELEVATION_REQUIRED
        // (os error 740) when a child installer needs admin rights but the
        // process tree was started unelevated.
        //
        // Scoped to release builds only: an elevated debug/test binary
        // can't be launched by `cargo run`/`cargo test` from a non-admin
        // shell (the exact same 740 error), which would break local dev.
        if std::env::var("PROFILE").as_deref() == Ok("release") {
            resource.set_manifest(
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="requireAdministrator" uiAccess="false" />
      </requestedPrivileges>
    </security>
  </trustInfo>
</assembly>
"#,
            );
        }
        resource.compile().expect("compile Windows resources");
    }
}
