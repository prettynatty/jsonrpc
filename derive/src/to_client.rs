use crate::rpc_attr::AttributeKind;
use crate::rpc_trait::crate_name;
use crate::to_delegate::{generate_where_clause_serialization_predicates, MethodRegistration};
use proc_macro2::{Ident, TokenStream};
use quote::quote;
use syn::punctuated::Punctuated;
use syn::Result;

pub fn generate_client_module(methods: &[MethodRegistration], item_trait: &syn::ItemTrait) -> Result<TokenStream> {
	let client_methods = generate_client_methods(methods)?;
	let generics = &item_trait.generics;
	let where_clause = generate_where_clause_serialization_predicates(&item_trait, true);
	let where_clause2 = where_clause.clone();
	let markers = generics
		.params
		.iter()
		.filter_map(|param| match param {
			syn::GenericParam::Type(syn::TypeParam { ident, .. }) => Some(ident),
			_ => None,
		})
		.enumerate()
		.map(|(i, ty)| {
			let field_name = "_".to_string() + &i.to_string();
			let field = Ident::new(&field_name, ty.span());
			(field, ty)
		});
	let (markers_decl, markers_impl): (Vec<_>, Vec<_>) = markers
		.map(|(field, ty)| {
			(
				quote! {
					#field: std::marker::PhantomData<#ty>
				},
				quote! {
					#field: std::marker::PhantomData
				},
			)
		})
		.unzip();
	let client_name = crate_name("jsonrpc-core-client")?;
	Ok(quote! {
		/// The generated client module.
		pub mod gen_client {
			use #client_name as _jsonrpc_core_client;
			use super::*;
			use _jsonrpc_core::{
				Call, Error, ErrorCode, Id, MethodCall, Params, Request,
				Response, Version,
			};
			use _jsonrpc_core::futures::prelude::*;
			use _jsonrpc_core::futures::sync::{mpsc, oneshot};
			use _jsonrpc_core::serde_json::{self, Value};
			use _jsonrpc_core_client::{RpcChannel, RpcError, RpcFuture, TypedClient, TypedSubscriptionStream};

			/// The Client.
			#[derive(Clone)]
			pub struct Client#generics {
				inner: TypedClient,
				#(#markers_decl),*
			}

			impl#generics Client#generics
			where
				#(#where_clause),*
			{
				/// Creates a new `Client`.
				pub fn new(sender: RpcChannel) -> Self {
					Client {
						inner: sender.into(),
						#(#markers_impl),*
					}
				}

				#(#client_methods)*
			}

			impl#generics From<RpcChannel> for Client#generics
			where
				#(#where_clause2),*
			{
				fn from(channel: RpcChannel) -> Self {
					Client::new(channel.into())
				}
			}
		}
	})
}

fn generate_client_methods(methods: &[MethodRegistration]) -> Result<Vec<syn::ImplItem>> {
	let mut client_methods = vec![];
	for method in methods {
		match method {
			MethodRegistration::Standard { method, .. } => {
				let attrs = get_doc_comments(&method.trait_item.attrs);
				let rpc_name = method.name();
				let name = &method.trait_item.sig.ident;
				let args = compute_args(&method.trait_item);
				let arg_names = compute_arg_identifiers(&args)?;
				let returns = match &method.attr.kind {
					AttributeKind::Rpc { returns, .. } => compute_returns(&method.trait_item, returns)?,
					AttributeKind::PubSub { .. } => continue,
				};
				let returns_str = quote!(#returns).to_string();
				let client_method = syn::parse_quote! {
					#(#attrs)*
					pub fn #name(&self, #args) -> impl Future<Item=#returns, Error=RpcError> {
						let args_tuple = (#(#arg_names,)*);
						self.inner.call_method(#rpc_name, #returns_str, args_tuple)
					}
				};
				client_methods.push(client_method);
			}
			MethodRegistration::PubSub {
				name: subscription,
				subscribe,
				unsubscribe,
			} => {
				let attrs = get_doc_comments(&subscribe.trait_item.attrs);
				let name = &subscribe.trait_item.sig.ident;
				let mut args = compute_args(&subscribe.trait_item).into_iter();
				let returns = compute_subscription_type(&args.next().unwrap());
				let returns_str = quote!(#returns).to_string();
				let args = args.collect();
				let arg_names = compute_arg_identifiers(&args)?;
				let subscribe = subscribe.name();
				let unsubscribe = unsubscribe.name();
				let client_method = syn::parse_quote!(
					#(#attrs)*
					pub fn #name(&self, #args) -> impl Future<Item=TypedSubscriptionStream<#returns>, Error=RpcError> {
						let args_tuple = (#(#arg_names,)*);
						self.inner.subscribe(#subscribe, args_tuple, #subscription, #unsubscribe, #returns_str)
					}
				);
				client_methods.push(client_method);
			}
			MethodRegistration::Notification { method, .. } => {
				let attrs = get_doc_comments(&method.trait_item.attrs);
				let rpc_name = method.name();
				let name = &method.trait_item.sig.ident;
				let args = compute_args(&method.trait_item);
				let arg_names = compute_arg_identifiers(&args)?;
				let client_method = syn::parse_quote! {
					#(#attrs)*
					pub fn #name(&self, #args) -> impl Future<Item = (), Error = RpcError> {
						let args_tuple = (#(#arg_names,)*);
						self.inner.notify(#rpc_name, args_tuple)
					}
				};
				client_methods.push(client_method);
			}
		}
	}
	Ok(client_methods)
}

fn get_doc_comments(attrs: &[syn::Attribute]) -> Vec<syn::Attribute> {
	let mut doc_comments = vec![];
	for attr in attrs {
		match attr {
			syn::Attribute {
				path: syn::Path { segments, .. },
				..
			} => match &segments[0] {
				syn::PathSegment { ident, .. } => {
					if ident.to_string() == "doc" {
						doc_comments.push(attr.to_owned());
					}
				}
			},
		}
	}
	doc_comments
}

fn compute_args(method: &syn::TraitItemMethod) -> Punctuated<syn::FnArg, syn::token::Comma> {
	let mut args = Punctuated::new();
	for arg in &method.sig.decl.inputs {
		let ty = match arg {
			syn::FnArg::Captured(syn::ArgCaptured { ty, .. }) => ty,
			_ => continue,
		};
		let segments = match ty {
			syn::Type::Path(syn::TypePath {
				path: syn::Path { segments, .. },
				..
			}) => segments,
			_ => continue,
		};
		let ident = match &segments[0] {
			syn::PathSegment { ident, .. } => ident,
		};
		if ident.to_string() == "Self" {
			continue;
		}
		args.push(arg.to_owned());
	}
	args
}

fn compute_arg_identifiers(args: &Punctuated<syn::FnArg, syn::token::Comma>) -> Result<Vec<&syn::Ident>> {
	let mut arg_names = vec![];
	for arg in args {
		let pat = match arg {
			syn::FnArg::Captured(syn::ArgCaptured { pat, .. }) => pat,
			_ => continue,
		};
		let ident = match pat {
			syn::Pat::Ident(syn::PatIdent { ident, .. }) => ident,
			syn::Pat::Wild(wild) => {
				let span = wild.underscore_token.spans[0];
				let msg = "No wildcard patterns allowed in rpc trait.";
				return Err(syn::Error::new(span, msg));
			}
			_ => continue,
		};
		arg_names.push(ident);
	}
	Ok(arg_names)
}

fn compute_returns(method: &syn::TraitItemMethod, returns: &Option<String>) -> Result<syn::Type> {
	let returns: Option<syn::Type> = match returns {
		Some(returns) => Some(syn::parse_str(returns)?),
		None => None,
	};
	let returns = match returns {
		None => try_infer_returns(&method.sig.decl.output),
		_ => returns,
	};
	let returns = match returns {
		Some(returns) => returns,
		None => {
			let span = method.attrs[0].pound_token.spans[0];
			let msg = "Missing returns attribute.";
			return Err(syn::Error::new(span, msg));
		}
	};
	Ok(returns)
}

fn try_infer_returns(output: &syn::ReturnType) -> Option<syn::Type> {
	match output {
		syn::ReturnType::Type(_, ty) => match &**ty {
			syn::Type::Path(syn::TypePath {
				path: syn::Path { segments, .. },
				..
			}) => match &segments[0] {
				syn::PathSegment { ident, arguments, .. } => {
					if ident.to_string().ends_with("Result") {
						get_first_type_argument(arguments)
					} else {
						None
					}
				}
			},
			_ => None,
		},
		_ => None,
	}
}

fn get_first_type_argument(args: &syn::PathArguments) -> Option<syn::Type> {
	match args {
		syn::PathArguments::AngleBracketed(syn::AngleBracketedGenericArguments { args, .. }) => {
			if args.len() > 0 {
				match &args[0] {
					syn::GenericArgument::Type(ty) => Some(ty.to_owned()),
					_ => None,
				}
			} else {
				None
			}
		}
		_ => None,
	}
}

fn compute_subscription_type(arg: &syn::FnArg) -> syn::Type {
	let ty = match arg {
		syn::FnArg::Captured(cap) => match &cap.ty {
			syn::Type::Path(path) => {
				let last = &path.path.segments[&path.path.segments.len() - 1];
				get_first_type_argument(&last.arguments)
			}
			_ => None,
		},
		_ => None,
	};
	ty.expect("a subscription needs a return type")
}
