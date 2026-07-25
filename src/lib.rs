/*!
Globalize makes global mutable variables practical by injecting them as static
mutable references into main:

```rust
use globalize::globals;

#[globals(
    s1 = String::new(),
    s2 = String::new(),
)]
fn main(s1: &'static mut String, s2: &'static mut String) {
    *s1 = "Hello".to_owned();
    *s2 = "World".to_owned();

    // Because these references are static, they can be shared with other
    // threads
    let t1 = std::thread::spawn(|| {
        s1.len() + s2.len()
    });

    let t2 = std::thread::spawn(|| {
        s1.len() * s2.len()
    });

    let out1 = t1.join().unwrap();
    let out2 = t2.join().unwrap();

    assert_eq!(out1 + out2, 35);
}
```

The disadvantage, of course, is that the references are only available to main
and must be passed as arguments or by capture to other functions. However, being
static references, they are much easier to pass to other threads or async tasks,
and can allow you to avoid polluting type signatures with lifetime annotations.

# Safety

`main`, as it turns out, is re-entrant (it's possible to call `main()` in your
program). `globalize` therefore inserts an extra check that panics if `main` is
called more than once. For absolute peak performance, can avoid this check by
adding `unsafe: nonreentrant;` to the attribute.

```rust
use globalize::globals;

#[globals(
    unsafe: nonreentrant;
    foo = String::new(),
)]
fn main(foo: &'static mut String) {}
```
*/

use std::{collections::HashMap, mem};

use proc_macro::TokenStream;
use quote::ToTokens;
use syn::{
    Attribute, FnArg, Ident, Pat, Stmt, Token,
    parse::{Parse, ParseStream},
    punctuated::Punctuated,
    spanned::Spanned as _,
};

struct GlobalItem {
    name: Ident,
    init: syn::Expr,
}

impl Parse for GlobalItem {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let name = input.parse()?;
        let _eq: Token![=] = input.parse()?;
        let init = input.parse()?;

        Ok(Self { name, init })
    }
}

struct GlobalSpec {
    skip_sync: bool,
    items: Punctuated<GlobalItem, Token![,]>,
}

impl GlobalSpec {
    pub fn statement_count(&self) -> usize {
        self.items.len() + if self.skip_sync { 1 } else { 0 }
    }
}

impl Parse for GlobalSpec {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        // First try to parse `unsafe: nonreentrant`
        let _unsafe: Option<Token![unsafe]> = input.parse()?;

        let skip_sync = _unsafe
            .map(|_| {
                let _colon: Token![:] = input.parse()?;
                let id: Ident = input.parse()?;
                let _semi: Token![;] = input.parse()?;

                if id == "nonreentrant" {
                    Ok(true)
                } else {
                    Err(syn::Error::new(id.span(), "expected 'reentrant'"))
                }
            })
            .transpose()?
            .unwrap_or(false);

        Punctuated::parse_terminated(input).map(|items| Self { skip_sync, items })
    }
}

fn reject_attrs<'a>(attrs: impl IntoIterator<Item = &'a Attribute>) -> syn::Result<()> {
    let error = attrs
        .into_iter()
        .map(|attr| syn::Error::new(attr.span(), "unexpected attribute"))
        .reduce(|mut accum, err| {
            accum.combine(err);
            accum
        });

    match error {
        None => Ok(()),
        Some(err) => Err(err),
    }
}

fn pat_ident(pat: &Pat) -> Option<Ident> {
    match *pat {
        Pat::Ident(ref pat)
            if pat.attrs.is_empty()
                && pat.by_ref.is_none()
                && pat.mutability.is_none()
                && pat.subpat.is_none() =>
        {
            Some(pat.ident.clone())
        }
        Pat::Paren(ref pat) => pat_ident(&*pat.pat),
        _ => None,
    }
}

fn unpack_static_mut_ref(ty: &syn::Type) -> syn::Result<&syn::Type> {
    match ty {
        syn::Type::Reference(ty) => {
            reject_attrs(&ty.attrs)?;

            if ty.mutability.is_none() {
                Err(syn::Error::new(
                    ty.span(),
                    "expected a mutable reference to this global",
                ))
            } else if ty.lifetime.as_ref().is_none_or(|lt| lt.ident != "static") {
                Err(syn::Error::new(
                    ty.span(),
                    "expected a 'static lifetime for this global",
                ))
            } else {
                Ok(&*ty.elem)
            }
        }
        syn::Type::Group(group) => unpack_static_mut_ref(&group.elem),
        syn::Type::Paren(group) => unpack_static_mut_ref(&group.elem),
        ty => Err(syn::Error::new(ty.span(), "expected &'static mut")),
    }
}

fn globals_impl(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let spec: GlobalSpec = syn::parse(attr)?;
    let mut func: syn::ItemFn = syn::parse(item)?;

    if func.sig.ident != "main" {
        return Err(syn::Error::new(
            func.sig.span(),
            "#[globals] should only be applied to `fn main`",
        ));
    }

    let mut unused_items: HashMap<String, &GlobalItem> = spec
        .items
        .iter()
        .map(|item| (item.name.to_string(), item))
        .collect();

    let args = mem::take(&mut func.sig.inputs);

    let mut injected: Vec<Stmt> = Vec::with_capacity(spec.statement_count());

    if !spec.skip_sync {
        injected.push(syn::parse_quote! {
            {
                static SEEN: ::core::sync::atomic::AtomicBool = ::core::sync::atomic::AtomicBool::new(false);
                if SEEN.swap(true, ::core::sync::atomic::Ordering::Relaxed) {
                    ::core::panic!("called main more than once")
                }
            };
        });
    }

    for arg in args {
        let arg = match arg {
            FnArg::Typed(arg) => arg,
            FnArg::Receiver(arg) => {
                func.sig.inputs.push(FnArg::Receiver(arg));
                continue;
            }
        };

        let Some(arg_ident) = pat_ident(&arg.pat) else {
            func.sig.inputs.push(FnArg::Typed(arg));
            continue;
        };

        let ident = &arg_ident;
        let name = ident.to_string();

        let Some(item) = unused_items.remove(name.as_str()) else {
            func.sig.inputs.push(FnArg::Typed(arg));
            continue;
        };

        let init = &item.init;
        let ty = unpack_static_mut_ref(&*arg.ty)?;

        injected.push(syn::parse_quote! {
            let #ident: &'static mut #ty = {
                static mut GLOBAL: ::core::mem::MaybeUninit<#ty> = ::core::mem::MaybeUninit::uninit();
                // Safety: unless the user unsafely opted out, this was
                // preceeded by a check of a global bool that ensure the
                // function can only be called once, so only one mutable
                // reference to the global can be produced.
                let global_ref = unsafe { &mut GLOBAL };
                ::core::mem::MaybeUninit::write(global_ref, #init)
            };
        });
    }

    if let Some(unused_err) = unused_items
        .values()
        .map(|unused| {
            syn::Error::new(
                unused.name.span(),
                "this global didn't appear in the arguments list",
            )
        })
        .reduce(|mut accum, err| {
            accum.combine(err);
            accum
        })
    {
        return Err(unused_err);
    }

    let mut stmts = injected;
    stmts.extend(func.block.stmts);
    func.block.stmts = stmts;

    Ok(func.to_token_stream().into())
}

/**
Inject global variables into your `main` function as static mutable references.
See [crate docs][crate] for details.
*/
#[proc_macro_attribute]
pub fn globals(attr: TokenStream, item: TokenStream) -> TokenStream {
    globals_impl(attr, item).unwrap_or_else(|err| err.into_compile_error().into())
}
