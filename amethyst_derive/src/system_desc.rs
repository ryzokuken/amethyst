//! PrefabData Implementation

use heck::SnakeCase;
use proc_macro2::{Literal, Span, TokenStream};
use proc_macro_roids::{DeriveInputStructExt, FieldExt};
use quote::quote;
use syn::{
    parse_quote, punctuated::Pair, AngleBracketedGenericArguments, Attribute, DeriveInput, Expr,
    Field, Fields, FieldsNamed, FieldsUnnamed, GenericArgument, GenericParam, Ident, ImplGenerics,
    LifetimeDef, Lit, Meta, MetaList, NestedMeta, Path, PathArguments, Type, TypeGenerics,
    TypePath, WhereClause,
};

pub fn impl_system_desc(ast: &DeriveInput) -> TokenStream {
    let system_name = &ast.ident;
    let system_desc_name = system_desc_name(&ast);

    // Whether the `SystemDesc` implementation is on the `System` type itself.
    let is_self = system_desc_name.is_none();
    let system_desc_name = system_desc_name.unwrap_or_else(|| system_name.clone());

    let (system_desc_fields, is_default) = if is_self {
        (SystemDescFields::default(), false)
    } else {
        let system_desc_fields = system_desc_fields(&ast);

        // Don't have to worry about fields to compute -- those are computed in the `build`
        // function.
        let is_default = system_desc_fields
            .field_mappings
            .iter()
            .find(|&field_mapping| {
                if let FieldVariant::Passthrough { .. } = field_mapping.field_variant {
                    true
                } else {
                    false
                }
            })
            .is_none();

        (system_desc_fields, is_default)
    };
    let (impl_generics, ty_generics, where_clause) = ast.generics.split_for_impl();

    let context = Context {
        system_name,
        system_desc_name,
        system_desc_fields,
        impl_generics,
        ty_generics,
        where_clause,
        is_default,
        is_self,
    };

    let (system_desc_struct, constructor, call_system_constructor) = if is_self {
        (TokenStream::new(), TokenStream::new(), quote!(self))
    } else {
        (
            system_desc_struct(&context),
            impl_constructor(&context),
            call_system_constructor(&context),
        )
    };
    let resource_insertion_expressions = resource_insertion_expressions(&ast);
    let field_computation_expressions = field_computation_expressions(&context.system_desc_fields);

    let Context {
        system_name,
        system_desc_name,
        ty_generics,
        where_clause,
        ..
    } = context;

    let mut generics = ast.generics.clone();
    let system_desc_life_a: LifetimeDef = parse_quote!('system_desc_a);
    let system_desc_life_b: LifetimeDef = parse_quote!('system_desc_b);
    generics
        .params
        .push(GenericParam::from(system_desc_life_a.clone()));
    generics
        .params
        .push(GenericParam::from(system_desc_life_b.clone()));
    let (impl_generics_with_lifetimes, _, _) = generics.split_for_impl();

    quote! {
        #system_desc_struct

        #constructor

        impl #impl_generics_with_lifetimes
        SystemDesc<
            #system_desc_life_a,
            #system_desc_life_b,
            #system_name #ty_generics
        >
            for #system_desc_name #ty_generics
        #where_clause
        {
            fn build(self, world: &mut World) -> #system_name #ty_generics {
                <#system_name #ty_generics as System<'_>>::SystemData::setup(world);

                #resource_insertion_expressions

                #field_computation_expressions

                #call_system_constructor
            }
        }
    }
}

fn system_desc_struct(context: &Context<'_>) -> TokenStream {
    let Context {
        ref system_name,
        ref system_desc_name,
        ref system_desc_fields,
        ref ty_generics,
        ref where_clause,
        ..
    } = context;

    let fields = &system_desc_fields.fields;
    let struct_declaration = match fields {
        Fields::Unit => quote!(struct #system_desc_name;),
        Fields::Unnamed(..) => quote! {
            struct #system_desc_name #ty_generics #fields #where_clause;
        },
        Fields::Named(..) => quote! {
            struct #system_desc_name #ty_generics #where_clause #fields
        },
    };

    let doc_string = format!("Builds a `{}`.", system_name);
    quote! {
        #[doc = #doc_string]
        #[derive(Debug)]
        pub #struct_declaration
    }
}

fn system_desc_fields(ast: &DeriveInput) -> SystemDescFields<'_> {
    // This includes any `PhantomData` fields to avoid unused type parameters.
    let fields = ast.fields();

    let mut system_desc_field_index = 0;
    let field_mappings = fields.iter().enumerate().fold(
        Vec::new(),
        |mut field_mappings, (system_field_index, field)| {
            let field_variant = if field.contains_tag("system_desc", "skip") {
                FieldVariant::Skipped(field)
            } else if field.contains_tag("system_desc", "event_channel_reader") {
                FieldVariant::Compute(FieldToCompute::ReaderId(field))
            } else if field.is_phantom_data() {
                let field_variant = FieldVariant::PhantomData {
                    system_desc_field_index,
                    field,
                };
                system_desc_field_index += 1;

                field_variant
            } else {
                let field_variant = FieldVariant::Passthrough {
                    system_desc_field_index,
                    field,
                };
                system_desc_field_index += 1;

                field_variant
            };

            let field_mapping = FieldMapping {
                system_field_index,
                field_variant,
            };
            field_mappings.push(field_mapping);

            field_mappings
        },
    );

    let fields = {
        let fields_to_copy = field_mappings
            .iter()
            .filter_map(|field_mapping| match &field_mapping.field_variant {
                FieldVariant::Skipped(..) | FieldVariant::Compute(..) => None,
                FieldVariant::PhantomData { field, .. }
                | FieldVariant::Passthrough { field, .. } => Some(*field),
            })
            .collect::<Vec<&Field>>();
        if fields_to_copy.is_empty() {
            Fields::Unit
        } else if ast.is_named() {
            let fields_named: FieldsNamed = parse_quote!({ #(#fields_to_copy,)* });
            Fields::from(fields_named)
        } else {
            // Tuple struct
            let fields_unnamed: FieldsUnnamed = parse_quote!((#(#fields_to_copy,)*));
            Fields::from(fields_unnamed)
        }
    };

    SystemDescFields {
        field_mappings,
        fields,
    }
}

fn impl_constructor(context: &Context<'_>) -> TokenStream {
    let Context {
        ref system_desc_name,
        ref impl_generics,
        ref ty_generics,
        ref where_clause,
        ref is_default,
        ..
    } = context;

    let constructor_parameters = impl_constructor_parameters(context);
    let constructor_body = impl_constructor_body(context);

    if *is_default {
        quote! {
            impl #impl_generics std::default::Default for #system_desc_name #ty_generics
            #where_clause
            {
                fn default() -> Self {
                    #constructor_body
                }
            }
        }
    } else {
        let doc_constructor = format!("Returns a new {}", system_desc_name);
        quote! {
            impl #impl_generics #system_desc_name #ty_generics
            #where_clause
            {
                #[doc = #doc_constructor]
                pub fn new(#constructor_parameters) -> Self {
                    #constructor_body
                }
            }
        }
    }
}

fn impl_constructor_body(context: &Context<'_>) -> TokenStream {
    let Context {
        ref system_desc_name,
        ref system_desc_fields,
        ..
    } = context;

    let fields = &system_desc_fields.fields;
    match fields {
        Fields::Unit => quote!(#system_desc_name),
        Fields::Unnamed(fields_unnamed) => {
            let field_initializers = fields_unnamed
                .unnamed
                .iter()
                .map(|field| {
                    if field.is_phantom_data() {
                        quote!(std::marker::PhantomData::default())
                    } else {
                        let type_name_snake = snake_case(field);
                        quote!(#type_name_snake)
                    }
                })
                .collect::<Vec<TokenStream>>();

            quote! {
                #system_desc_name(#(#field_initializers,)*)
            }
        }
        Fields::Named(fields_named) => {
            let field_initializers = fields_named
                .named
                .iter()
                .map(|field| {
                    let field_name = field
                        .ident
                        .as_ref()
                        .expect("Expected named field to have an ident.");

                    if field.is_phantom_data() {
                        quote!(#field_name: std::marker::PhantomData::default())
                    } else {
                        quote!(#field_name)
                    }
                })
                .collect::<Vec<TokenStream>>();

            quote! {
                #system_desc_name {
                    #(#field_initializers,)*
                }
            }
        }
    }
}

fn impl_constructor_parameters(context: &Context<'_>) -> TokenStream {
    let Context {
        ref system_desc_fields,
        ..
    } = context;

    let fields = &system_desc_fields.fields;
    match fields {
        Fields::Unit => quote!(),
        Fields::Unnamed(fields_unnamed) => {
            let constructor_parameters = fields_unnamed
                .unnamed
                .iter()
                .filter(|field| !field.is_phantom_data())
                .map(|field| {
                    let type_name_snake = snake_case(field);
                    let field_type = &field.ty;
                    quote!(#type_name_snake: #field_type)
                })
                .collect::<Vec<TokenStream>>();

            quote! {
                #(#constructor_parameters,)*
            }
        }
        Fields::Named(fields_named) => {
            let constructor_parameters = fields_named
                .named
                .iter()
                .filter(|field| !field.is_phantom_data())
                .map(|field| {
                    let field_name = field
                        .ident
                        .as_ref()
                        .expect("Expected named field to have an ident.");
                    let field_type = &field.ty;
                    quote!(#field_name: #field_type)
                })
                .collect::<Vec<TokenStream>>();

            quote! {
                #(#constructor_parameters,)*
            }
        }
    }
}

fn call_system_constructor(context: &Context<'_>) -> TokenStream {
    let Context {
        ref system_name,
        ref system_desc_fields,
        ..
    } = context;

    let fields = &system_desc_fields.fields;
    let field_mappings = &system_desc_fields.field_mappings;
    match fields {
        Fields::Unit => {
            // `SystemDesc` has no fields, but the `System` might.
            // If there are no fields to compute, then we call the `System` unit constructor.
            // If there are fields to compute, we call `System::new(..)`.
            // If there are skipped fields but no fields to compute, we call `System::default()`.

            if field_mappings.is_empty() {
                quote!(#system_name)
            } else {
                let has_fields_to_compute = field_mappings.iter().any(|field_mapping| {
                    if let FieldVariant::Compute(..) = &field_mapping.field_variant {
                        true
                    } else {
                        false
                    }
                });
                if has_fields_to_compute {
                    let field_initializers = field_mappings
                        .iter()
                        .filter_map(|field_mapping| match &field_mapping.field_variant {
                            FieldVariant::Skipped(..) => None,
                            FieldVariant::Compute(FieldToCompute::ReaderId(field)) => {
                                let field_name =
                                    field.ident.clone().unwrap_or_else(|| snake_case(field));
                                Some(quote!(#field_name))
                            }
                            FieldVariant::PhantomData { .. } => {
                                unreachable!(
                                    "`SystemDesc` will not have `Unit` fields \
                                     when a `PhantomData` field exists."
                                );
                            }
                            FieldVariant::Passthrough { .. } => {
                                unreachable!(
                                    "`SystemDesc` will not have `Unit` fields \
                                     when a `Passthrough` field exists."
                                );
                            }
                        })
                        .collect::<Vec<TokenStream>>();

                    quote! {
                        #system_name::new(#(#field_initializers,)*)
                    }
                } else {
                    quote!(#system_name::default())
                }
            }
        }
        Fields::Unnamed(..) => {
            let has_fields_to_compute = field_mappings.iter().any(|field_mapping| {
                if let FieldVariant::Compute(..) = &field_mapping.field_variant {
                    true
                } else {
                    false
                }
            });
            if has_fields_to_compute {
                let field_initializers = field_mappings
                    .iter()
                    .filter_map(|field_mapping| match &field_mapping.field_variant {
                        FieldVariant::Skipped(..) => None,
                        FieldVariant::Compute(FieldToCompute::ReaderId(field)) => {
                            let field_name = snake_case(field);
                            Some(quote!(#field_name))
                        }
                        FieldVariant::PhantomData { .. } => None,
                        FieldVariant::Passthrough {
                            system_desc_field_index,
                            ..
                        } => {
                            let index = Literal::usize_unsuffixed(*system_desc_field_index);
                            Some(quote!(self.#index))
                        }
                    })
                    .collect::<Vec<TokenStream>>();

                quote! {
                    #system_name::new(#(#field_initializers,)*)
                }
            } else {
                quote!(#system_name::default())
            }
        }
        Fields::Named(..) => {
            let has_fields_to_compute_or_passthrough =
                field_mappings
                    .iter()
                    .any(|field_mapping| match &field_mapping.field_variant {
                        FieldVariant::Compute(..) | FieldVariant::Passthrough { .. } => true,
                        _ => false,
                    });
            let has_fields_skipped_or_phantom =
                field_mappings
                    .iter()
                    .any(|field_mapping| match &field_mapping.field_variant {
                        FieldVariant::Skipped(..) | FieldVariant::PhantomData { .. } => true,
                        _ => false,
                    });
            if has_fields_to_compute_or_passthrough {
                if has_fields_skipped_or_phantom {
                    let field_initializers = field_mappings
                        .iter()
                        .filter_map(|field_mapping| match &field_mapping.field_variant {
                            FieldVariant::Skipped(..) => None,
                            FieldVariant::Compute(FieldToCompute::ReaderId(field)) => {
                                let field_name = field
                                    .ident
                                    .as_ref()
                                    .expect("Expected named field to have an ident.");
                                Some(quote!(#field_name))
                            }
                            FieldVariant::PhantomData { .. } => None,
                            FieldVariant::Passthrough { field, .. } => {
                                let field_name = field
                                    .ident
                                    .as_ref()
                                    .expect("Expected named field to have an ident.");
                                Some(quote!(self.#field_name))
                            }
                        })
                        .collect::<Vec<TokenStream>>();

                    quote! {
                        #system_name::new(#(#field_initializers,)*)
                    }
                } else {
                    let field_initializers = field_mappings
                        .iter()
                        .filter_map(|field_mapping| match &field_mapping.field_variant {
                            FieldVariant::Skipped(..) => None,
                            FieldVariant::Compute(FieldToCompute::ReaderId(field)) => {
                                let field_name = field
                                    .ident
                                    .as_ref()
                                    .expect("Expected named field to have an ident.");
                                Some(quote!(#field_name))
                            }
                            FieldVariant::PhantomData { .. } => None,
                            FieldVariant::Passthrough { field, .. } => {
                                let field_name = field
                                    .ident
                                    .as_ref()
                                    .expect("Expected named field to have an ident.");
                                Some(quote!(#field_name: self.#field_name))
                            }
                        })
                        .collect::<Vec<TokenStream>>();

                    quote! {
                        #system_name {
                            #(#field_initializers,)*
                        }
                    }
                }
            } else if has_fields_skipped_or_phantom {
                quote!(#system_name::default())
            } else {
                quote! {
                    #system_name {}
                }
            }
        }
    }
}

/// Extracts the name from the `#[system_desc(name(..))]` attribute.
#[allow(clippy::let_and_return)] // Needed due to bug in clippy.
fn system_desc_name(ast: &DeriveInput) -> Option<Ident> {
    let meta_lists = ast
        .attrs
        .iter()
        .map(Attribute::parse_meta)
        .filter_map(Result::ok)
        .filter(|meta| meta.name() == "system_desc")
        .filter_map(|meta| {
            if let Meta::List(meta_list) = meta {
                Some(meta_list)
            } else {
                None
            }
        })
        .collect::<Vec<MetaList>>();

    // Each `meta_list` is the `system_desc(..)` item.
    let name = meta_lists
        .iter()
        .flat_map(|meta_list| {
            meta_list
                .nested
                .iter()
                .filter_map(|nested_meta| {
                    if let NestedMeta::Meta(meta) = nested_meta {
                        Some(meta)
                    } else {
                        None
                    }
                })
                .filter(|meta| meta.name() == "name")
        })
        // `meta` is the `name(..)` item.
        .filter_map(|meta| {
            if let Meta::List(meta_list) = meta {
                Some(meta_list)
            } else {
                None
            }
        })
        // We want to insert a resource for each item in the list.
        .map(|meta_list| {
            if meta_list.nested.len() != 1 {
                panic!(
                    "Expected exactly one identifier for `#[system_desc(name(..))]`. `{:?}`.",
                    &meta_list.nested
                );
            }

            meta_list
                .nested
                .first()
                .map(|pair| {
                    let nested_meta = pair.value();
                    if let NestedMeta::Meta(Meta::Word(ident)) = nested_meta {
                        ident.clone()
                    } else {
                        panic!(
                            "`{:?}` is an invalid value in this position.\n\
                             Expected a single identifier.",
                            nested_meta,
                        );
                    }
                })
                .expect("Expected one meta item to exist.")
        })
        .next();

    name
}

/// Inserts resources specified inside the `#[system_desc(insert(..))]` attribute.
fn resource_insertion_expressions(ast: &DeriveInput) -> TokenStream {
    let meta_lists = ast
        .attrs
        .iter()
        .map(Attribute::parse_meta)
        .filter_map(Result::ok)
        .filter(|meta| meta.name() == "system_desc")
        .filter_map(|meta| {
            if let Meta::List(meta_list) = meta {
                Some(meta_list)
            } else {
                None
            }
        })
        .collect::<Vec<MetaList>>();

    // Each `meta_list` is the `system_desc(..)` item.
    meta_lists
        .iter()
        .flat_map(|meta_list| {
            meta_list
                .nested
                .iter()
                .filter_map(|nested_meta| {
                    if let NestedMeta::Meta(meta) = nested_meta {
                        Some(meta)
                    } else {
                        None
                    }
                })
                .filter(|meta| meta.name() == "insert")
        })
        // `meta` is the `insert(..)` item.
        .filter_map(|meta| {
            if let Meta::List(meta_list) = meta {
                Some(meta_list)
            } else {
                None
            }
        })
        // We want to insert a resource for each item in the list.
        .flat_map(|meta_list| {
            meta_list
                .nested
                .iter()
                .map(|nested_meta| match nested_meta {
                    NestedMeta::Meta(meta) => {
                        if let Meta::Word(ident) = meta {
                            quote!(#ident)
                        } else {
                            panic!(
                                "`{:?}` is an invalid value in this position.\n\
                                 Expected a literal string or single word.",
                                meta
                            )
                        }
                    }
                    NestedMeta::Literal(lit) => {
                        if let Lit::Str(lit_str) = lit {
                            // Turn the literal into tokens.
                            // The literal must be a valid expression
                            let expr = lit_str.parse::<Expr>().unwrap_or_else(|e| {
                                panic!(
                                    "Failed to parse `{:?}` as an expression. Error: {}",
                                    lit_str, e,
                                )
                            });
                            quote!(#expr)
                        } else {
                            panic!(
                                "`{:?}` is an invalid value in this position.\n\
                                 Expected a literal string or single word.",
                                lit
                            )
                        }
                    }
                })
        })
        .fold(TokenStream::new(), |mut accumulated_tokens, expr_tokens| {
            accumulated_tokens.extend(quote! {
                world.insert(#expr_tokens);
            });
            accumulated_tokens
        })
}

/// Computes resources from the `World`.
fn field_computation_expressions(system_desc_fields: &SystemDescFields<'_>) -> TokenStream {
    system_desc_fields.field_mappings.iter().fold(
        TokenStream::new(),
        |mut token_stream, field_mapping| {
            if let FieldMapping {
                field_variant: FieldVariant::Compute(FieldToCompute::ReaderId(field)),
                ..
            } = field_mapping
            {
                let field_name = field.ident.clone().unwrap_or_else(|| snake_case(field));
                let event_type_path = if let Type::Path(TypePath {
                    path: Path { segments, .. },
                    ..
                }) = &field.ty
                {
                    if let Some(Pair::End(path_segment)) = segments.last() {
                        if let PathArguments::AngleBracketed(AngleBracketedGenericArguments {
                            args,
                            ..
                        }) = &path_segment.arguments
                        {
                            if let Some(Pair::End(GenericArgument::Type(Type::Path(TypePath {
                                path,
                                ..
                            })))) = args.first()
                            {
                                path
                            } else {
                                panic!(
                                    "Expected `{}` first generic parameter to be a type.",
                                    &field_name
                                )
                            }
                        } else {
                            panic!("Expected `{}` field to have type parameters.", &field_name)
                        }
                    } else {
                        panic!("Expected `{}` field last segment to exist.", &field_name)
                    }
                } else {
                    panic!("Expected `{}` field type to be `Type::Path`.", &field_name)
                };
                let tokens = quote! {
                    let #field_name = world
                        .fetch_mut::<EventChannel<#event_type_path>>()
                        .register_reader();
                };
                token_stream.extend(tokens);
            }

            token_stream
        },
    )
}

fn snake_case(field: &Field) -> Ident {
    let type_name_snake = field.type_name().to_string().to_snake_case();
    Ident::new(&type_name_snake, Span::call_site())
}

#[derive(Debug)]
struct Context<'c> {
    system_name: &'c Ident,
    system_desc_name: Ident,
    system_desc_fields: SystemDescFields<'c>,
    impl_generics: ImplGenerics<'c>,
    ty_generics: TypeGenerics<'c>,
    where_clause: Option<&'c WhereClause>,
    is_default: bool,
    is_self: bool,
}

/// Disambiguation of fields from the `System`.
#[derive(Debug)]
struct SystemDescFields<'f> {
    /// Fields from `System`, with contextual information.
    field_mappings: Vec<FieldMapping<'f>>,
    /// Fields to copy across from the `System` struct, re-quoted and parsed.
    fields: Fields,
}

impl<'f> Default for SystemDescFields<'f> {
    fn default() -> Self {
        SystemDescFields {
            field_mappings: Vec::new(),
            fields: Fields::Unit,
        }
    }
}

/// Exists to track the index of the field on the `System` struct.
///
/// This allows the `SystemDesc` type to have different fields, but we retain the position
/// information to map from the `SystemDesc` struct to the `System`.
#[derive(Debug, PartialEq)]
struct FieldMapping<'f> {
    /// Position of the field on the `System` type.
    system_field_index: usize,
    /// `FieldVariant` of the `System` struct.
    field_variant: FieldVariant<'f>,
}

#[derive(Debug, PartialEq)]
enum FieldVariant<'f> {
    /// The field is skipped.
    Skipped(&'f Field),
    /// The field is to be computed from the `World`.
    Compute(FieldToCompute<'f>),
    /// Field is phantom data.
    ///
    /// This appears in both the `SystemDesc` and `System` structs, but are instantiated
    /// independently and not passed through.
    PhantomData {
        /// Position of the field on the `SystemDesc` type.
        system_desc_field_index: usize,
        /// `Field` information from the `System`.
        field: &'f Field,
    },
    /// Field is a parameter to pass through.
    Passthrough {
        /// Position of the field on the `SystemDesc` type.
        system_desc_field_index: usize,
        /// `Field` information from the `System`.
        field: &'f Field,
    },
}

#[derive(Debug, PartialEq)]
enum FieldToCompute<'f> {
    /// `ReaderId` from registering as a reader for an `EventChannel` in the `World`.
    ReaderId(&'f Field),
}
