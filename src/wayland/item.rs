use {
    crate::{
        sni::{SniItem, SniItemId, SniItemProperties},
        wayland::tray::item::{icon::IconTemplate, menu::Menu},
    },
    ahash::AHashMap,
    std::sync::Arc,
};

#[derive(Default)]
pub struct Items {
    pub items: AHashMap<SniItemId, Item>,
}

pub struct Item {
    pub sni: Arc<SniItem>,
    pub props: SniItemProperties,
    pub icon: IconTemplate,
    pub attention_icon: IconTemplate,
    pub menu: Menu,
}

impl Item {
    pub fn initialize(&mut self) {
        self.update_icon();
        self.update_attention_icon();
    }

    pub fn update_icon(&mut self) {
        self.icon.update_name(
            self.props.icon_name.as_ref(),
            self.props.icon_theme_path.as_ref(),
        );
        self.icon.update_frames(self.props.icon.as_ref());
    }

    pub fn update_attention_icon(&mut self) {
        self.attention_icon.update_name(
            self.props.attention_icon_name.as_ref(),
            self.props.icon_theme_path.as_ref(),
        );
        self.attention_icon
            .update_frames(self.props.attention_icon.as_ref());
    }
}
