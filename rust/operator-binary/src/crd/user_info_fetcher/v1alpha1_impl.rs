use crate::crd::user_info_fetcher::v1alpha1;

// TODO (@Techassi): Most of these impls are the exact same across v1alpha1 and v1alpha2. Explore
// and design a more elegant solution for it.
impl Default for v1alpha1::Backend {
    fn default() -> Self {
        Self::None {}
    }
}
