// Copyright 2019-2020
//     by  Centrality Investments Ltd.
//     and Parity Technologies (UK) Ltd.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

mod impl_wrapper;

use alloc::vec;
use alloc::vec::Vec;
use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{
	parse::{Error, Result},
	parse_quote,
	punctuated::Punctuated,
	token::Comma,
	visit_mut::VisitMut,
	Data, DataEnum, DataStruct, DeriveInput, Expr, ExprLit, Field, Fields, GenericArgument, Lifetime, Lit,
	PathArguments, Type, TypeParam, Variant,
};

#[proc_macro_derive(Metadata)]
pub fn metadata(input: TokenStream) -> TokenStream {
	match generate(input.into()) {
		Ok(output) => output.into(),
		Err(err) => err.to_compile_error().into(),
	}
}

fn generate(input: TokenStream2) -> Result<TokenStream2> {
	let mut tokens = quote! {};
	tokens.extend(generate_type(input)?);
	Ok(tokens)
}

fn generate_type(input: TokenStream2) -> Result<TokenStream2> {
	let mut ast: DeriveInput = syn::parse2(input.clone())?;

	ast.generics.type_params_mut().for_each(|p| {
		p.bounds.push(parse_quote!(_scale_info::Metadata));
		p.bounds.push(parse_quote!('static));
	});

	let mut static_lifetime_generics = ast.generics.clone();
	static_lifetime_generics
		.lifetimes_mut()
		.for_each(|l| *l = parse_quote!('static));

	let ident = &ast.ident;
	let impl_generics_no_lifetimes = ast.generics.type_params();
	let (_, _, where_clause) = ast.generics.split_for_impl();
	let (_, ty_generics, _) = static_lifetime_generics.split_for_impl();
	let generic_type_ids = ast.generics.type_params().map(|ty| &ty.ident);

	let type_params = ast.generics.type_params().collect::<Vec<_>>();
	let (type_kind, build_type) = match &ast.data {
		Data::Struct(ref s) => (quote!(TypeComposite), generate_composite_type(s, &type_params)),
		Data::Enum(ref e) => (quote!(TypeVariant), generate_variant_type(e, &type_params)),
		Data::Union(_) => return Err(Error::new_spanned(input, "Unions not supported")),
	};

	let meta_type_params =
		generic_type_ids.map(|tp| quote! { _scale_info::MetaTypeConcrete::new::<#tp>() });

	let type_info_impl = quote! {
		impl <#( #impl_generics_no_lifetimes ),*> _scale_info::TypeInfo for #ident #ty_generics #where_clause {
			fn path() -> _scale_info::Path {
				_scale_info::Path::new(stringify!(#ident), module_path!())
			}

			fn params() -> __core::Vec<_scale_info::MetaTypeConcrete> {
				__core::vec![
					#( #meta_type_params ),*
				]
			}

			fn type_info() -> _scale_info::Type {
				_scale_info::#type_kind::new(#build_type).into()
			}
		}
	};

	Ok(impl_wrapper::wrap(ident, "TYPE_INFO", type_info_impl))
}

type FieldsList = Punctuated<Field, Comma>;

fn generate_fields(fields: &FieldsList, type_params: &[&TypeParam]) -> Vec<TokenStream2> {
	fields.iter().map(|f| generate_field(f, type_params)).collect()
}

fn generate_field(field: &Field, type_params: &[&TypeParam]) -> TokenStream2 {
	let (ty, ident) = (&field.ty, &field.ident);

	// Replace any field lifetime params with `static to prevent "unnecessary lifetime parameter"
	// warning. Any lifetime parameters are specified as 'static in the type of the impl.
	struct StaticLifetimesReplace;
	impl VisitMut for StaticLifetimesReplace {
		fn visit_lifetime_mut(&mut self, lifetime: &mut Lifetime) {
			*lifetime = parse_quote!('static)
		}
	}
	let mut ty = ty.clone();
	StaticLifetimesReplace.visit_type_mut(&mut ty);

	let (field_method, field_args) = if let Some(ty) = get_type_parameter(&ty, type_params) {
		// it's a field of a parameter e.g. `a: T`
		(quote!( .parameter_field::<Self, #ty> ), quote!(stringify!(#ty)))
	} else {
		let type_params = generate_parameterized_field_parameters(&ty, type_params, true);
		if type_params.is_empty() {
			// it's a concrete non-generic type
			(quote!( .field_of::<#ty> ), quote!())
		} else {
			let parameters = quote! {
				__core::vec![
					#( #type_params ),*
				]
			};
			(quote!( .parameterized_field::<#ty> ), quote!( #parameters ))
		}
	};

	if let Some(i) = ident {
		// it's a named field, assumes the field name is the first argument to the field method
		quote! {
			#field_method(stringify!(#i), #field_args)
		}
	} else {
		// it's an unnamed field
		quote! {
			#field_method(#field_args)
		}
	}
}

fn get_type_parameter(ty: &Type, type_params: &[&TypeParam]) -> Option<Type> {
	match ty {
		Type::Path(path) => {
			if type_params.iter().any(|tp| Some(&tp.ident) == path.path.get_ident()) {
				Some(ty.clone())
			} else {
				None
			}
		}
		// references to type params are just the plain type param (`&'a T` and `&'a mut T`) -> T
		Type::Reference(reference) => Some(*reference.elem.clone()),
		_ => None,
	}
}

fn generate_parameterized_field_parameters(ty: &Type, type_params: &[&TypeParam], is_root: bool) -> Vec<TokenStream2> {
	if let Some(ty) = get_type_parameter(ty, type_params) {
		return vec![quote! {
			_scale_info::MetaTypeParameterValue::parameter::<Self, #ty>(stringify!(#ty))
		}];
	}

	match ty {
		Type::Path(path) => {
			if let Some(segment) = path.path.segments.last() {
				match &segment.arguments {
					PathArguments::None => {
						if is_root {
							Vec::new()
						} else {
							vec![quote! {
								_scale_info::MetaTypeParameterValue::concrete::<#ty>()
							}]
						}
					}
					PathArguments::AngleBracketed(args) => args
						.args
						.iter()
						.flat_map(|arg| match arg {
							GenericArgument::Type(ty) => {
								generate_parameterized_field_parameters(ty, type_params, false)
							}
							_ => Vec::new(),
						})
						.collect(),
					PathArguments::Parenthesized(args) => args
						.inputs
						.iter()
						.flat_map(|arg_ty| generate_parameterized_field_parameters(arg_ty, type_params, false))
						.collect(),
				}
			} else {
				Vec::new()
			}
		}
		Type::Tuple(tuple) => tuple
			.elems
			.iter()
			.flat_map(|ty| generate_parameterized_field_parameters(ty, type_params, false))
			.collect(),
		Type::Array(array) => generate_parameterized_field_parameters(&array.elem, type_params, false),
		_ => Vec::new(), // todo: handle references, slices and any other parameterized types
	}
}

fn generate_composite_type(data_struct: &DataStruct, type_params: &[&TypeParam]) -> TokenStream2 {
	match data_struct.fields {
		Fields::Named(ref fs) => {
			let fields = generate_fields(&fs.named, type_params);
			quote! {
				_scale_info::Fields::named()
					#( #fields )*
			}
		}
		Fields::Unnamed(ref fs) => {
			let fields = generate_fields(&fs.unnamed, type_params);
			quote! {
				_scale_info::Fields::unnamed()
					#( #fields )*
			}
		}
		Fields::Unit => quote! {
			_scale_info::Fields::unit()
		},
	}
}

type VariantList = Punctuated<Variant, Comma>;

fn generate_c_like_enum_def(variants: &VariantList) -> TokenStream2 {
	let variants = variants.into_iter().enumerate().map(|(i, v)| {
		let name = &v.ident;
		let discriminant = if let Some((
			_,
			Expr::Lit(ExprLit {
				lit: Lit::Int(lit_int), ..
			}),
		)) = &v.discriminant
		{
			match lit_int.base10_parse::<u64>() {
				Ok(i) => i,
				Err(err) => return err.to_compile_error(),
			}
		} else {
			i as u64
		};
		quote! {
			.variant(stringify!(#name), #discriminant)
		}
	});
	quote! {
		_scale_info::Variants::with_discriminants()
			#( #variants )*
	}
}

fn is_c_like_enum(variants: &VariantList) -> bool {
	// any variant has an explicit discriminant
	variants.iter().any(|v| v.discriminant.is_some()) ||
		// all variants are unit
		variants.iter().all(|v| match v.fields {
			Fields::Unit => true,
			_ => false,
		})
}

fn generate_variant_type(data_enum: &DataEnum, type_params: &[&TypeParam]) -> TokenStream2 {
	let variants = &data_enum.variants;

	if is_c_like_enum(&variants) {
		return generate_c_like_enum_def(variants);
	}

	let variants = variants.into_iter().map(|v| {
		let ident = &v.ident;
		let v_name = quote! { stringify!(#ident) };
		match v.fields {
			Fields::Named(ref fs) => {
				let fields = generate_fields(&fs.named, type_params);
				quote! {
					.variant(
						#v_name,
						_scale_info::Fields::named()
							#( #fields)*
					)
				}
			}
			Fields::Unnamed(ref fs) => {
				let fields = generate_fields(&fs.unnamed, type_params);
				quote! {
					.variant(
						#v_name,
						_scale_info::Fields::unnamed()
							#( #fields)*
					)
				}
			}
			Fields::Unit => quote! {
				.variant_unit(#v_name)
			},
		}
	});
	quote! {
		_scale_info::Variants::with_fields()
			#( #variants)*
	}
}
