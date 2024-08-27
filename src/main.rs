use yew::prelude::*;

pub struct Button {
    label: String,
    onclick: Callback<()>,
}

#[derive(Properties, PartialEq)]
pub struct ButtonProps {
    pub label: String,
    pub onclick: Callback<()>,
}

impl Component for Button {
    type Message = ();
    type Properties = ButtonProps;

    fn create(ctx: &Context<Self>) -> Self {
        Self {
            label: ctx.props().label.clone(),
            onclick: ctx.props().onclick.clone(),
        }
    }

    fn update(&mut self, _ctx: &Context<Self>, _msg: Self::Message) -> bool {
        false
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let onclick = ctx.link().callback(|_| {
            self.onclick.emit(());
        });

        html! {
            <button {onclick}>{ &self.label }</button>
        }
    }
}

#[function_component(App)]
fn app() -> Html {
    let open_directory = Callback::from(|| {
        //open directory
    });

    html! {
        <Button label="Add" {open_directory} />
    }
}

fn main() {}
