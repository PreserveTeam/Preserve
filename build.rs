fn main() {
    #[cfg(windows)]
    {
        let mut resource = winres::WindowsResource::new();
        resource.set_icon("assets/preserve.ico");
        resource.compile().expect("compile Windows resources");
    }
}
