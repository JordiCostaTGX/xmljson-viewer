use crate::document::Document;
use gtk::{gio, glib, prelude::*, subclass::prelude::*};
use std::{cell::RefCell, collections::HashMap, sync::Arc};

#[derive(Clone)]
pub struct NodeRef {
    pub document: Arc<Document>,
    pub id: usize,
    pub group: Option<Arc<[usize]>>,
    pub label: String,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct LazyNodes {
        pub data: RefCell<Option<(Arc<Document>, Arc<[usize]>)>>,
        pub cache: RefCell<HashMap<u32, glib::WeakRef<glib::BoxedAnyObject>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LazyNodes {
        const NAME: &'static str = "XmlJsonLazyNodes";
        type Type = super::LazyNodes;
        type Interfaces = (gio::ListModel,);
    }

    impl ObjectImpl for LazyNodes {}

    impl ListModelImpl for LazyNodes {
        fn item_type(&self) -> glib::Type {
            glib::BoxedAnyObject::static_type()
        }

        fn n_items(&self) -> u32 {
            self.data.borrow().as_ref().map_or(0, |(_, ids)| {
                if ids.len() > 512 {
                    ids.len().div_ceil(512) as u32
                } else {
                    ids.len() as u32
                }
            })
        }

        fn item(&self, position: u32) -> Option<glib::Object> {
            let data = self.data.borrow();
            let (document, ids) = data.as_ref()?;
            if let Some(object) = self
                .cache
                .borrow()
                .get(&position)
                .and_then(|weak| weak.upgrade())
            {
                return Some(object.upcast());
            }
            let node = if ids.len() > 512 {
                let start = position as usize * 512;
                if start >= ids.len() {
                    return None;
                }
                let end = (start + 512).min(ids.len());
                NodeRef {
                    document: document.clone(),
                    id: usize::MAX,
                    group: Some(Arc::from(&ids[start..end])),
                    label: format!("Items {}–{}", start + 1, end),
                }
            } else {
                let &id = ids.get(position as usize)?;
                NodeRef {
                    document: document.clone(),
                    id,
                    group: None,
                    label: document.nodes[id].label.clone(),
                }
            };
            let object = glib::BoxedAnyObject::new(node);
            let mut cache = self.cache.borrow_mut();
            if cache.len() > 2048 {
                cache.retain(|_, weak| weak.upgrade().is_some());
            }
            cache.insert(position, object.downgrade());
            Some(object.upcast())
        }
    }
}

glib::wrapper! {
    pub struct LazyNodes(ObjectSubclass<imp::LazyNodes>) @implements gio::ListModel;
}

impl LazyNodes {
    pub fn new(document: Arc<Document>, ids: Arc<[usize]>) -> Self {
        let model: Self = glib::Object::new();
        model.imp().data.replace(Some((document, ids)));
        model
    }
}

pub fn model(document: Arc<Document>) -> gtk::TreeListModel {
    let roots = LazyNodes::new(document.clone(), document.roots.clone());
    gtk::TreeListModel::new(roots, false, false, |object| {
        let object = object.downcast_ref::<glib::BoxedAnyObject>()?;
        let node = object.borrow::<NodeRef>();
        let children = node
            .group
            .as_ref()
            .unwrap_or_else(|| &node.document.nodes[node.id].children);
        if children.is_empty() {
            None
        } else {
            Some(LazyNodes::new(node.document.clone(), children.clone()).upcast())
        }
    })
}

pub fn node_at(model: &gtk::TreeListModel, index: u32) -> Option<NodeRef> {
    let row = model.row(index)?;
    let object = row.item()?.downcast::<glib::BoxedAnyObject>().ok()?;
    Some(object.borrow::<NodeRef>().clone())
}

pub fn reveal(model: &gtk::TreeListModel, document: &Document, id: usize) -> Option<u32> {
    let mut ancestors = Vec::new();
    let mut parent = document.nodes[id].parent;
    while let Some(id) = parent {
        ancestors.push(id);
        parent = document.nodes[id].parent;
    }
    for ancestor in ancestors.into_iter().rev().chain(std::iter::once(id)) {
        let mut position = 0;
        while position < model.n_items() {
            let node = node_at(model, position)?;
            if node
                .group
                .as_ref()
                .is_some_and(|ids| ids.contains(&ancestor))
            {
                model.row(position)?.set_expanded(true);
            }
            if node.id == ancestor {
                if ancestor == id {
                    return Some(position);
                }
                model.row(position)?.set_expanded(true);
                break;
            }
            position += 1;
        }
    }
    None
}
