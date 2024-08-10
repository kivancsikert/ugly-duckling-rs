#[macro_export]
macro_rules! make_static {
    ($type:ty) => {{
        static SINGLETON: static_cell::StaticCell<$type> = static_cell::StaticCell::new();
        SINGLETON.init(<$type>::new())
    }};
}
