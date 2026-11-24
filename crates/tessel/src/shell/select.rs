use super::*;
use gpui_kit::component::Icon;
use gpui_kit::component::popover::Popover;
use std::cell::Cell;
use std::rc::Rc;

pub(crate) type Pick = Rc<dyn Fn(&mut Shell, &str, &mut Window, &mut Context<Shell>)>;

pub(crate) struct Select {
    pub id: &'static str,
    pub label: String,
    pub options: Vec<(String, String)>,
    pub current: String,
    pub width: Option<f32>,
    pub height: f32,
    pub up: bool,
}

impl Shell {
    fn select_cell(&self, id: &'static str) -> Rc<Cell<f32>> {
        self.select_w.borrow_mut().entry(id).or_default().clone()
    }

    pub(crate) fn select(&self, s: Select, pick: Pick, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let cell = self.select_cell(s.id);
        let (measure, wide) = (cell.clone(), cell);
        let me = cx.entity().downgrade();
        let Select { id, label, options, current, width, height, up } = s;
        let popover = Popover::new(SharedString::from(format!("{id}-popover")))
            .rounded(theme::z(theme::R_FLOAT))
            .p_0()
            .anchor(if up { gpui_kit::Anchor::BottomLeft } else { gpui_kit::Anchor::TopLeft })
            .trigger(
                super::composer::tool_button(id)
                    .w_full()
                    .h(theme::z(height))
                    .px(theme::z(theme::SIDE_PAD + 2.))
                    .rounded(theme::z(theme::by_height(height)))
                    .bg(p.fill)
                    .child(
                        h_flex()
                            .w_full()
                            .gap(theme::z(8.))
                            .items_center()
                            .child(div().flex_1().min_w_0().truncate().text_left().text_size(theme::z(14.)).text_color(p.label).child(label))
                            .child(Icon::empty().path("icons/chevron-down.svg").size(theme::z(13.)).text_color(p.secondary)),
                    ),
            )
            .content(move |_, _, cx| {
                let popover = cx.entity();
                let Some(shell) = me.upgrade() else { return div().into_any_element() };
                let p = shell.read(cx).palette.get();
                let mut rows = v_flex();
                for (i, (value, text)) in options.iter().enumerate() {
                    let (me, popover, pick, value) = (me.clone(), popover.clone(), pick.clone(), value.clone());
                    rows = rows.child(
                        theme::float_item(&p, format!("{id}-option-{i}"), None, text.clone(), None, *value == current)
                            .on_click(move |_, window, cx| {
                                popover.update(cx, |st, cx| st.dismiss(window, cx));
                                let _ = me.update(cx, |this, cx| {
                                    pick(this, &value, window, cx);
                                    cx.notify();
                                });
                            })
                            .test_support(),
                    );
                }
                let w = wide.get();
                theme::float_list(&p).when(w > 0., |d| d.w(px(w))).child(rows).into_any_element()
            });
        div()
            .relative()
            .flex_shrink_0()
            .map(|d| match width {
                Some(w) => d.w(theme::z(w)),
                None => d.w_full(),
            })
            .child(popover)
            .child(canvas(move |b, _, _| measure.set(f32::from(b.size.width)), |_, _, _, _| {}).absolute().inset_0())
            .into_any_element()
    }
}
