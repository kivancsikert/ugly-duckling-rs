#[macro_export]
macro_rules! make_static {
    ($type:ty, $init:expr) => {{
        static SINGLETON: static_cell::StaticCell<$type> = static_cell::StaticCell::new();
        SINGLETON.init($init)
    }};
}
