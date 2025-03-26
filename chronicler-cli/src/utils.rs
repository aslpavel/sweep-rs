use sweep::surf_n_term::{
    Face,
    view::{Align, Container, Flex, IntoView},
};

pub(crate) struct Table<'a> {
    left_width: usize,
    left_face: Option<Face>,
    right_face: Option<Face>,
    view: Flex<'a>,
}

impl<'a> Table<'a> {
    pub(crate) fn new(
        left_width: usize,
        left_face: Option<Face>,
        right_face: Option<Face>,
    ) -> Self {
        Self {
            left_width,
            left_face,
            right_face,
            view: Flex::column(),
        }
    }

    pub(crate) fn push(&mut self, left: impl IntoView + 'a, right: impl IntoView + 'a) {
        let row = Flex::row()
            .add_child_ext(
                Container::new(left)
                    .with_width(self.left_width)
                    .with_horizontal(Align::Expand),
                None,
                self.left_face,
                Align::Start,
            )
            .add_child_ext(right, None, self.right_face, Align::Start);
        self.view.push_child(row)
    }
}

impl<'a> IntoView for Table<'a> {
    type View = Flex<'a>;

    fn into_view(self) -> Self::View {
        self.view
    }
}
