fn main() {
    let codec = pkg_config::Config::new().probe("libavcodec").unwrap();
    let util = pkg_config::Config::new().probe("libavutil").unwrap();
    let pipewire = pkg_config::Config::new().probe("libpipewire-0.3").unwrap();
    let mut build = cc::Build::new();
    build.file("src/audio/eld_ffmpeg.c").file("src/linux/micsource.c");
    for path in codec.include_paths.iter().chain(&util.include_paths).chain(&pipewire.include_paths)
    {
        build.include(path);
    }
    build.compile("fleetpods_media");
}
