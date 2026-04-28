use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{parse_macro_input, Attribute, Data, DeriveInput, Field, Fields, Lit, Type};

pub fn derive_event_impl(input: TokenStream) -> TokenStream {
    let derive_input = parse_macro_input!(input as DeriveInput);

    match generate_event_impl(&derive_input) {
        Ok(token_stream) => token_stream.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

struct ParsedEventAttrs {
    subject_template: String,
    /// User-provided value for `aggregate = "..."`; omit expands to `"default"`.
    aggregate: Option<String>,
}

fn parse_event_attrs(attr: &Attribute) -> syn::Result<ParsedEventAttrs> {
    let mut subject_template = None::<String>;
    let mut aggregate = None::<String>;

    attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("subject") {
            let value = meta.value()?;
            let lit: Lit = value.parse()?;
            if let Lit::Str(literal_string) = lit {
                subject_template = Some(literal_string.value());
                return Ok(());
            }
            return Err(meta.error("subject must be a string literal"));
        }

        if meta.path.is_ident("aggregate") {
            let value = meta.value()?;
            let lit: Lit = value.parse()?;
            if let Lit::Str(literal_string) = lit {
                aggregate = Some(literal_string.value());
                return Ok(());
            }
            return Err(meta.error("aggregate must be a string literal"));
        }

        Err(meta.error("unknown event attribute key"))
    })?;

    let subject_template = subject_template.ok_or_else(|| {
        syn::Error::new_spanned(attr, "subject = \"...\" is required in #[event(...)]")
    })?;

    Ok(ParsedEventAttrs {
        subject_template,
        aggregate,
    })
}

fn generate_event_impl(derive_input: &DeriveInput) -> syn::Result<TokenStream2> {
    let type_name = &derive_input.ident;
    let event_attribute = derive_input
        .attrs
        .iter()
        .find(|attribute| attribute.path().is_ident("event"))
        .ok_or_else(|| {
            syn::Error::new_spanned(type_name, "missing #[event(subject = \"...\")] attribute")
        })?;

    let ParsedEventAttrs {
        subject_template,
        aggregate,
    } = parse_event_attrs(event_attribute)?;

    let fields = match &derive_input.data {
        Data::Struct(data_struct) => match &data_struct.fields {
            Fields::Named(named_fields) => &named_fields.named,
            _ => {
                return Err(syn::Error::new_spanned(
                    type_name,
                    "#[derive(Event)] requires a struct with named fields",
                ));
            }
        },
        _ => {
            return Err(syn::Error::new_spanned(
                type_name,
                "#[derive(Event)] can only be applied to structs",
            ));
        }
    };

    let id_field = fields
        .iter()
        .find(|field| field.ident.as_ref().is_some_and(|name| name == "id"));

    let Some(id_field) = id_field else {
        return Err(syn::Error::new_spanned(
            type_name,
            "#[derive(Event)] requires a field named `id: bus_core::MessageId`",
        ));
    };

    if !is_message_id_field(id_field) {
        return Err(syn::Error::new_spanned(
            id_field,
            "#[derive(Event)] requires `id` to have type `bus_core::MessageId`",
        ));
    }

    let field_idents: Vec<&syn::Ident> = fields.iter().filter_map(|f| f.ident.as_ref()).collect();
    let subject_expression =
        build_subject_expression(&subject_template, type_name, &field_idents)?;

    let aggregate_quoted = match aggregate.as_deref() {
        Some(value) => quote! { #value },
        None => quote! { "default" },
    };

    Ok(quote! {
        impl bus_core::Event for #type_name {
            fn subject(&self) -> ::std::borrow::Cow<'_, str> {
                ::std::borrow::Cow::Owned(#subject_expression)
            }

            fn message_id(&self) -> bus_core::MessageId {
                self.id.clone()
            }

            fn aggregate_type() -> &'static str {
                #aggregate_quoted
            }
        }
    })
}

/// Accepts `MessageId` or `bus_core::MessageId` (or `::bus_core::MessageId`), rejects other paths
/// ending in `MessageId` (e.g. `other::MessageId`).
fn is_message_id_field(field: &Field) -> bool {
    let Type::Path(type_path) = &field.ty else {
        return false;
    };

    if type_path.qself.is_some() {
        return false;
    }

    let segments = &type_path.path.segments;
    let Some(last) = segments.last() else {
        return false;
    };

    if last.ident != "MessageId" {
        return false;
    }

    match segments.len() {
        1 => true,
        _ => segments[segments.len() - 2].ident == "bus_core",
    }
}

fn build_subject_expression(
    subject_template: &str,
    span_target: &syn::Ident,
    field_idents: &[&syn::Ident],
) -> syn::Result<TokenStream2> {
    let placeholder_count = subject_template.bytes().filter(|b| *b == b'{').count();
    let mut format_string = String::with_capacity(
        subject_template
            .len()
            .saturating_add(placeholder_count.saturating_mul(2)),
    );

    let mut interpolation_arguments: Vec<TokenStream2> = Vec::with_capacity(placeholder_count);

    let mut rest = subject_template;

    while let Some(open) = rest.find('{') {
        format_string.push_str(&rest[..open]);
        rest = &rest[open + 1..];

        let close = rest.find('}').ok_or_else(|| {
            syn::Error::new_spanned(span_target, "unclosed `{` in subject template")
        })?;

        let interpolation = rest[..close].trim();
        rest = &rest[close + 1..];

        if !interpolation.starts_with("self.") {
            return Err(syn::Error::new_spanned(
                span_target,
                format!(
                    "subject interpolations must use `{{self.field}}` form, got `{{{interpolation}}}`"
                ),
            ));
        }

        let field_name = &interpolation["self.".len()..];
        let field_ident: syn::Ident = syn::parse_str(field_name).map_err(|_| {
            syn::Error::new_spanned(
                span_target,
                format!("invalid field name in subject template: `{field_name}`"),
            )
        })?;

        if !field_idents.iter().any(|known| **known == field_ident) {
            return Err(syn::Error::new_spanned(
                span_target,
                format!("unknown field in subject template: `{field_name}`"),
            ));
        }

        format_string.push_str("{}");
        interpolation_arguments.push(quote! { self.#field_ident });
    }

    format_string.push_str(rest);

    if interpolation_arguments.is_empty() {
        return Ok(quote! { #format_string.to_owned() });
    }

    Ok(quote! { format!(#format_string, #(#interpolation_arguments),*) })
}
