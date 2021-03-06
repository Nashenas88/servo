/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// `data` comes from components/style/properties.mako.rs; see build.rs for more details.

<%!
    from data import to_rust_ident
    from data import Keyword
%>

use app_units::Au;
use custom_properties::ComputedValuesMap;
% for style_struct in data.style_structs:
use gecko_bindings::structs::${style_struct.gecko_ffi_name};
use gecko_bindings::bindings::Gecko_Construct_${style_struct.gecko_ffi_name};
use gecko_bindings::bindings::Gecko_CopyConstruct_${style_struct.gecko_ffi_name};
use gecko_bindings::bindings::Gecko_Destroy_${style_struct.gecko_ffi_name};
% endfor
use gecko_bindings::bindings::{Gecko_CopyMozBindingFrom, Gecko_CopyListStyleTypeFrom};
use gecko_bindings::bindings::{Gecko_SetMozBinding, Gecko_SetListStyleType};
use gecko_bindings::bindings::{Gecko_SetNullImageValue, Gecko_SetGradientImageValue};
use gecko_bindings::bindings::{Gecko_EnsureImageLayersLength, Gecko_CreateGradient};
use gecko_bindings::bindings::{Gecko_CopyImageValueFrom, Gecko_CopyFontFamilyFrom};
use gecko_bindings::bindings::{Gecko_FontFamilyList_AppendGeneric, Gecko_FontFamilyList_AppendNamed};
use gecko_bindings::bindings::{Gecko_FontFamilyList_Clear, Gecko_InitializeImageLayer};
use gecko_bindings::bindings;
use gecko_bindings::structs;
use gecko_bindings::sugar::ns_style_coord::{CoordDataValue, CoordData, CoordDataMut};
use gecko_glue::ArcHelpers;
use gecko_values::{StyleCoordHelpers, GeckoStyleCoordConvertible, convert_nscolor_to_rgba};
use gecko_values::convert_rgba_to_nscolor;
use gecko_values::round_border_to_device_pixels;
use logical_geometry::WritingMode;
use properties::CascadePropertyFn;
use properties::longhands;
use std::fmt::{self, Debug};
use std::mem::{transmute, uninitialized, zeroed};
use std::ptr;
use std::sync::atomic::{ATOMIC_USIZE_INIT, AtomicUsize, Ordering};
use std::sync::Arc;
use std::cmp;

pub mod style_structs {
    % for style_struct in data.style_structs:
    pub use super::${style_struct.gecko_struct_name} as ${style_struct.name};
    % endfor
}

#[derive(Clone, Debug)]
pub struct ComputedValues {
    % for style_struct in data.style_structs:
    ${style_struct.ident}: Arc<style_structs::${style_struct.name}>,
    % endfor

    custom_properties: Option<Arc<ComputedValuesMap>>,
    shareable: bool,
    pub writing_mode: WritingMode,
    pub root_font_size: Au,
}

impl ComputedValues {
    pub fn inherit_from(parent: &Arc<Self>) -> Arc<Self> {
        Arc::new(ComputedValues {
            custom_properties: parent.custom_properties.clone(),
            shareable: parent.shareable,
            writing_mode: parent.writing_mode,
            root_font_size: parent.root_font_size,
            % for style_struct in data.style_structs:
            % if style_struct.inherited:
            ${style_struct.ident}: parent.${style_struct.ident}.clone(),
            % else:
            ${style_struct.ident}: Self::initial_values().${style_struct.ident}.clone(),
            % endif
            % endfor
        })
    }

    pub fn new(custom_properties: Option<Arc<ComputedValuesMap>>,
           shareable: bool,
           writing_mode: WritingMode,
           root_font_size: Au,
            % for style_struct in data.style_structs:
           ${style_struct.ident}: Arc<style_structs::${style_struct.name}>,
            % endfor
    ) -> Self {
        ComputedValues {
            custom_properties: custom_properties,
            shareable: shareable,
            writing_mode: writing_mode,
            root_font_size: root_font_size,
            % for style_struct in data.style_structs:
            ${style_struct.ident}: ${style_struct.ident},
            % endfor
        }
    }

    pub fn style_for_child_text_node(parent: &Arc<Self>) -> Arc<Self> {
        // Gecko expects text nodes to be styled as if they were elements that
        // matched no rules (that is, inherited style structs are inherited and
        // non-inherited style structs are set to their initial values).
        ComputedValues::inherit_from(parent)
    }

    pub fn initial_values() -> &'static Self {
        unsafe {
            debug_assert!(!raw_initial_values().is_null());
            &*raw_initial_values()
        }
    }

    pub unsafe fn initialize() {
        debug_assert!(raw_initial_values().is_null());
        set_raw_initial_values(Box::into_raw(Box::new(ComputedValues {
            % for style_struct in data.style_structs:
               ${style_struct.ident}: style_structs::${style_struct.name}::initial(),
            % endfor
            custom_properties: None,
            shareable: true,
            writing_mode: WritingMode::empty(),
            root_font_size: longhands::font_size::get_initial_value(),
        })));
    }

    pub unsafe fn shutdown() {
        debug_assert!(!raw_initial_values().is_null());
        let _ = Box::from_raw(raw_initial_values());
        set_raw_initial_values(ptr::null_mut());
    }

    #[inline]
    pub fn do_cascade_property<F: FnOnce(&[CascadePropertyFn])>(f: F) {
        f(&CASCADE_PROPERTY)
    }

    % for style_struct in data.style_structs:
    #[inline]
    pub fn clone_${style_struct.name_lower}(&self) -> Arc<style_structs::${style_struct.name}> {
        self.${style_struct.ident}.clone()
    }
    #[inline]
    pub fn get_${style_struct.name_lower}(&self) -> &style_structs::${style_struct.name} {
        &self.${style_struct.ident}
    }
    #[inline]
    pub fn mutate_${style_struct.name_lower}(&mut self) -> &mut style_structs::${style_struct.name} {
        Arc::make_mut(&mut self.${style_struct.ident})
    }
    % endfor

    pub fn custom_properties(&self) -> Option<Arc<ComputedValuesMap>> {
        self.custom_properties.as_ref().map(|x| x.clone())
    }

    pub fn root_font_size(&self) -> Au { self.root_font_size }
    pub fn set_root_font_size(&mut self, s: Au) { self.root_font_size = s; }
    pub fn set_writing_mode(&mut self, mode: WritingMode) { self.writing_mode = mode; }

    // FIXME(bholley): Implement this properly.
    #[inline]
    pub fn is_multicol(&self) -> bool { false }
}

<%def name="declare_style_struct(style_struct)">
pub struct ${style_struct.gecko_struct_name} {
    gecko: ${style_struct.gecko_ffi_name},
}
</%def>

<%def name="impl_simple_setter(ident, gecko_ffi_name)">
    #[allow(non_snake_case)]
    pub fn set_${ident}(&mut self, v: longhands::${ident}::computed_value::T) {
        ${set_gecko_property(gecko_ffi_name, "v")}
    }
</%def>

<%def name="impl_simple_clone(ident, gecko_ffi_name)">
    #[allow(non_snake_case)]
    pub fn clone_${ident}(&self) -> longhands::${ident}::computed_value::T {
        self.gecko.${gecko_ffi_name}
    }
</%def>

<%def name="impl_simple_copy(ident, gecko_ffi_name, *kwargs)">
    #[allow(non_snake_case)]
    pub fn copy_${ident}_from(&mut self, other: &Self) {
        self.gecko.${gecko_ffi_name} = other.gecko.${gecko_ffi_name};
    }
</%def>

<%def name="impl_coord_copy(ident, gecko_ffi_name)">
    #[allow(non_snake_case)]
    pub fn copy_${ident}_from(&mut self, other: &Self) {
        self.gecko.${gecko_ffi_name}.copy_from(&other.gecko.${gecko_ffi_name});
    }
</%def>

<%!
def is_border_style_masked(ffi_name):
    return ffi_name.split("[")[0] in ["mBorderStyle", "mOutlineStyle", "mTextDecorationStyle"]

def get_gecko_property(ffi_name):
    if is_border_style_masked(ffi_name):
        return "(self.gecko.%s & (structs::BORDER_STYLE_MASK as u8))" % ffi_name
    return "self.gecko.%s" % ffi_name

def set_gecko_property(ffi_name, expr):
    if is_border_style_masked(ffi_name):
        return "self.gecko.%s &= !(structs::BORDER_STYLE_MASK as u8);" % ffi_name + \
               "self.gecko.%s |= %s as u8;" % (ffi_name, expr)
    elif ffi_name == "__LIST_STYLE_TYPE__":
        return "unsafe { Gecko_SetListStyleType(&mut self.gecko, %s as u32); }" % expr
    return "self.gecko.%s = %s;" % (ffi_name, expr)
%>

<%def name="impl_keyword_setter(ident, gecko_ffi_name, keyword)">
    #[allow(non_snake_case)]
    pub fn set_${ident}(&mut self, v: longhands::${ident}::computed_value::T) {
        use properties::longhands::${ident}::computed_value::T as Keyword;
        // FIXME(bholley): Align binary representations and ditch |match| for cast + static_asserts
        let result = match v {
            % for value in keyword.values_for('gecko'):
            Keyword::${to_rust_ident(value)} => structs::${keyword.gecko_constant(value)} ${keyword.maybe_cast("u8")},
            % endfor
        };
        ${set_gecko_property(gecko_ffi_name, "result")}
    }
</%def>

<%def name="impl_keyword_clone(ident, gecko_ffi_name, keyword)">
    #[allow(non_snake_case)]
    pub fn clone_${ident}(&self) -> longhands::${ident}::computed_value::T {
        use properties::longhands::${ident}::computed_value::T as Keyword;
        // FIXME(bholley): Align binary representations and ditch |match| for cast + static_asserts
        match ${get_gecko_property(gecko_ffi_name)} ${keyword.maybe_cast("u32")} {
            % for value in keyword.values_for('gecko'):
            structs::${keyword.gecko_constant(value)} => Keyword::${to_rust_ident(value)},
            % endfor
            x => panic!("Found unexpected value in style struct for ${ident} property: {:?}", x),
        }
    }
</%def>

<%def name="clear_color_flags(color_flags_ffi_name)">
    % if color_flags_ffi_name:
    self.gecko.${color_flags_ffi_name} &= !(structs::BORDER_COLOR_SPECIAL as u8);
    % endif
</%def>

<%def name="set_current_color_flag(color_flags_ffi_name)">
    % if color_flags_ffi_name:
    self.gecko.${color_flags_ffi_name} |= structs::BORDER_COLOR_FOREGROUND as u8;
    % else:
    // FIXME(heycam): This is a Gecko property that doesn't store currentColor
    // as a computed value.  These are currently handled by converting
    // currentColor to the current value of the color property at computed
    // value time, but we don't have access to the Color struct here.
    // In the longer term, Gecko should store currentColor as a computed
    // value, so that we don't need to do this:
    // https://bugzilla.mozilla.org/show_bug.cgi?id=760345
    warn!("stylo: mishandling currentColor");
    % endif
</%def>

<%def name="get_current_color_flag_from(field)">
    (${field} & (structs::BORDER_COLOR_FOREGROUND as u8)) != 0
</%def>

<%def name="impl_color_setter(ident, gecko_ffi_name, color_flags_ffi_name=None)">
    #[allow(unreachable_code)]
    #[allow(non_snake_case)]
    pub fn set_${ident}(&mut self, v: longhands::${ident}::computed_value::T) {
        use cssparser::Color;
        ${clear_color_flags(color_flags_ffi_name)}
        let result = match v {
            Color::CurrentColor => {
                ${set_current_color_flag(color_flags_ffi_name)}
                0
            },
            Color::RGBA(rgba) => convert_rgba_to_nscolor(&rgba),
        };
        ${set_gecko_property(gecko_ffi_name, "result")}
    }
</%def>

<%def name="impl_color_copy(ident, gecko_ffi_name, color_flags_ffi_name=None)">
    #[allow(non_snake_case)]
    pub fn copy_${ident}_from(&mut self, other: &Self) {
        % if color_flags_ffi_name:
            ${clear_color_flags(color_flags_ffi_name)}
            if ${get_current_color_flag_from("other.gecko." + color_flags_ffi_name)} {
                ${set_current_color_flag(color_flags_ffi_name)}
            }
        % endif
        self.gecko.${gecko_ffi_name} = other.gecko.${gecko_ffi_name}
    }
</%def>

<%def name="impl_color_clone(ident, gecko_ffi_name, color_flags_ffi_name=None)">
    #[allow(non_snake_case)]
    pub fn clone_${ident}(&self) -> longhands::${ident}::computed_value::T {
        use cssparser::Color;
        % if color_flags_ffi_name:
            if ${get_current_color_flag_from("self.gecko." + color_flags_ffi_name)} {
                return Color::CurrentColor
            }
        % endif
        Color::RGBA(convert_nscolor_to_rgba(${get_gecko_property(gecko_ffi_name)}))
    }
</%def>

<%def name="impl_keyword(ident, gecko_ffi_name, keyword, need_clone)">
<%call expr="impl_keyword_setter(ident, gecko_ffi_name, keyword)"></%call>
<%call expr="impl_simple_copy(ident, gecko_ffi_name)"></%call>
%if need_clone:
<%call expr="impl_keyword_clone(ident, gecko_ffi_name, keyword)"></%call>
% endif
</%def>

<%def name="impl_simple(ident, gecko_ffi_name, need_clone=False)">
<%call expr="impl_simple_setter(ident, gecko_ffi_name)"></%call>
<%call expr="impl_simple_copy(ident, gecko_ffi_name)"></%call>
% if need_clone:
    <%call expr="impl_simple_clone(ident, gecko_ffi_name)"></%call>
% endif
</%def>

<%def name="impl_color(ident, gecko_ffi_name, color_flags_ffi_name=None, need_clone=False)">
<%call expr="impl_color_setter(ident, gecko_ffi_name, color_flags_ffi_name)"></%call>
<%call expr="impl_color_copy(ident, gecko_ffi_name, color_flags_ffi_name)"></%call>
% if need_clone:
    <%call expr="impl_color_clone(ident, gecko_ffi_name, color_flags_ffi_name)"></%call>
% endif
</%def>

<%def name="impl_app_units(ident, gecko_ffi_name, need_clone, round_to_pixels=False)">
    #[allow(non_snake_case)]
    pub fn set_${ident}(&mut self, v: longhands::${ident}::computed_value::T) {
        % if round_to_pixels:
        let au_per_device_px = Au(self.gecko.mTwipsPerPixel);
        self.gecko.${gecko_ffi_name} = round_border_to_device_pixels(v, au_per_device_px).0;
        % else:
        self.gecko.${gecko_ffi_name} = v.0;
        % endif
    }
<%call expr="impl_simple_copy(ident, gecko_ffi_name)"></%call>
%if need_clone:
    #[allow(non_snake_case)]
    pub fn clone_${ident}(&self) -> longhands::${ident}::computed_value::T {
        Au(self.gecko.${gecko_ffi_name})
    }
% endif
</%def>

<%def name="impl_split_style_coord(ident, gecko_ffi_name, index, need_clone=False)">
    #[allow(non_snake_case)]
    pub fn set_${ident}(&mut self, v: longhands::${ident}::computed_value::T) {
        v.to_gecko_style_coord(&mut self.gecko.${gecko_ffi_name}.data_at_mut(${index}));
    }
    #[allow(non_snake_case)]
    pub fn copy_${ident}_from(&mut self, other: &Self) {
        self.gecko.${gecko_ffi_name}.data_at_mut(${index}).copy_from(&other.gecko.${gecko_ffi_name}.data_at(${index}));
    }
    % if need_clone:
        #[allow(non_snake_case)]
        pub fn clone_${ident}(&self) -> longhands::${ident}::computed_value::T {
            use properties::longhands::${ident}::computed_value::T;
            T::from_gecko_style_coord(&self.gecko.${gecko_ffi_name}.data_at(${index}))
                .expect("clone for ${ident} failed")
        }
    % endif
</%def>

<%def name="impl_style_coord(ident, gecko_ffi_name, need_clone=False)">
    #[allow(non_snake_case)]
    pub fn set_${ident}(&mut self, v: longhands::${ident}::computed_value::T) {
        v.to_gecko_style_coord(&mut self.gecko.${gecko_ffi_name});
    }
    #[allow(non_snake_case)]
    pub fn copy_${ident}_from(&mut self, other: &Self) {
        self.gecko.${gecko_ffi_name}.copy_from(&other.gecko.${gecko_ffi_name});
    }
    % if need_clone:
        #[allow(non_snake_case)]
        pub fn clone_${ident}(&self) -> longhands::${ident}::computed_value::T {
            use properties::longhands::${ident}::computed_value::T;
            T::from_gecko_style_coord(&self.gecko.${gecko_ffi_name})
                .expect("clone for ${ident} failed")
        }
    % endif
</%def>

<%def name="impl_corner_style_coord(ident, gecko_ffi_name, x_index, y_index, need_clone=False)">
    #[allow(non_snake_case)]
    pub fn set_${ident}(&mut self, v: longhands::${ident}::computed_value::T) {
        v.0.width.to_gecko_style_coord(&mut self.gecko.${gecko_ffi_name}.data_at_mut(${x_index}));
        v.0.height.to_gecko_style_coord(&mut self.gecko.${gecko_ffi_name}.data_at_mut(${y_index}));
    }
    #[allow(non_snake_case)]
    pub fn copy_${ident}_from(&mut self, other: &Self) {
        self.gecko.${gecko_ffi_name}.data_at_mut(${x_index})
                  .copy_from(&other.gecko.${gecko_ffi_name}.data_at(${x_index}));
        self.gecko.${gecko_ffi_name}.data_at_mut(${y_index})
                  .copy_from(&other.gecko.${gecko_ffi_name}.data_at(${y_index}));
    }
    % if need_clone:
        #[allow(non_snake_case)]
        pub fn clone_${ident}(&self) -> longhands::${ident}::computed_value::T {
            use properties::longhands::${ident}::computed_value::T;
            use euclid::Size2D;
            let width = GeckoStyleCoordConvertible::from_gecko_style_coord(
                            &self.gecko.${gecko_ffi_name}.data_at(${x_index}))
                            .expect("Failed to clone ${ident}");
            let height = GeckoStyleCoordConvertible::from_gecko_style_coord(
                            &self.gecko.${gecko_ffi_name}.data_at(${y_index}))
                            .expect("Failed to clone ${ident}");
            T(Size2D::new(width, height))
        }
    % endif
</%def>

<%def name="impl_style_struct(style_struct)">
impl ${style_struct.gecko_struct_name} {
    #[allow(dead_code, unused_variables)]
    pub fn initial() -> Arc<Self> {
        let mut result = Arc::new(${style_struct.gecko_struct_name} { gecko: unsafe { zeroed() } });
        unsafe {
            Gecko_Construct_${style_struct.gecko_ffi_name}(&mut Arc::make_mut(&mut result).gecko);
        }
        result
    }
    pub fn get_gecko(&self) -> &${style_struct.gecko_ffi_name} {
        &self.gecko
    }
}
impl Drop for ${style_struct.gecko_struct_name} {
    fn drop(&mut self) {
        unsafe {
            Gecko_Destroy_${style_struct.gecko_ffi_name}(&mut self.gecko);
        }
    }
}
impl Clone for ${style_struct.gecko_struct_name} {
    fn clone(&self) -> Self {
        unsafe {
            let mut result = ${style_struct.gecko_struct_name} { gecko: zeroed() };
            Gecko_CopyConstruct_${style_struct.gecko_ffi_name}(&mut result.gecko, &self.gecko);
            result
        }
    }
}

// FIXME(bholley): Make bindgen generate Debug for all types.
%if style_struct.gecko_ffi_name in ("nsStyle" + x for x in "Border Display List Background Font SVGReset".split()):
impl Debug for ${style_struct.gecko_struct_name} {
    // FIXME(bholley): Generate this.
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "GECKO STYLE STRUCT")
    }
}
%else:
impl Debug for ${style_struct.gecko_struct_name} {
    // FIXME(bholley): Generate this.
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result { self.gecko.fmt(f) }
}
%endif
</%def>

<%def name="raw_impl_trait(style_struct, skip_longhands='', skip_additionals='')">
<%
   longhands = [x for x in style_struct.longhands
                if not (skip_longhands == "*" or x.name in skip_longhands.split())]

   #
   # Make a list of types we can't auto-generate.
   #
   force_stub = [];
   # These are currently being shuffled to a different style struct on the gecko side.
   force_stub += ["backface-visibility", "transform-box", "transform-style"]
   # These live in an nsFont member in Gecko. Should be straightforward to do manually.
   force_stub += ["font-kerning", "font-stretch", "font-variant"]
   # These have unusual representations in gecko.
   force_stub += ["list-style-type", "text-overflow"]
   # In a nsTArray, have to be done manually, but probably not too much work
   # (the "filling them", not the "making them work")
   force_stub += ["animation-name", "animation-duration",
                  "animation-timing-function", "animation-iteration-count",
                  "animation-direction", "animation-play-state",
                  "animation-fill-mode", "animation-delay"]

   # Types used with predefined_type()-defined properties that we can auto-generate.
   predefined_types = {
       "LengthOrPercentage": impl_style_coord,
       "LengthOrPercentageOrAuto": impl_style_coord,
       "LengthOrPercentageOrNone": impl_style_coord,
       "Number": impl_simple,
       "Opacity": impl_simple,
   }

   keyword_longhands = [x for x in longhands if x.keyword and not x.name in force_stub]
   predefined_longhands = [x for x in longhands
                           if x.predefined_type in predefined_types and not x.name in force_stub]
   stub_longhands = [x for x in longhands if x not in keyword_longhands + predefined_longhands]
%>
impl ${style_struct.gecko_struct_name} {
    /*
     * Manually-Implemented Methods.
     */
    ${caller.body().strip()}

    /*
     * Auto-Generated Methods.
     */
    <%
    for longhand in keyword_longhands:
        impl_keyword(longhand.ident, longhand.gecko_ffi_name, longhand.keyword, longhand.need_clone)
    for longhand in predefined_longhands:
        impl_fn = predefined_types[longhand.predefined_type]
        impl_fn(longhand.ident, longhand.gecko_ffi_name, need_clone=longhand.need_clone)
    %>

    /*
     * Stubs.
     */
    % for longhand in stub_longhands:
    #[allow(non_snake_case)]
    pub fn set_${longhand.ident}(&mut self, _: longhands::${longhand.ident}::computed_value::T) {
        if cfg!(debug_assertions) {
            println!("stylo: Unimplemented property setter: ${longhand.name}");
        }
    }
    #[allow(non_snake_case)]
    pub fn copy_${longhand.ident}_from(&mut self, _: &Self) {
        if cfg!(debug_assertions) {
            println!("stylo: Unimplemented property setter: ${longhand.name}");
        }
    }
    % if longhand.need_clone:
    #[allow(non_snake_case)]
    pub fn clone_${longhand.ident}(&self) -> longhands::${longhand.ident}::computed_value::T {
        unimplemented!()
    }
    % endif
    % if longhand.need_index:
    pub fn ${longhand.ident}_count(&self) -> usize { 0 }
    pub fn ${longhand.ident}_at(&self, _index: usize)
                                -> longhands::${longhand.ident}::computed_value::SingleComputedValue {
        unimplemented!()
    }
    % endif
    % endfor
    <% additionals = [x for x in style_struct.additional_methods
                      if skip_additionals != "*" and not x.name in skip_additionals.split()] %>
    % for additional in additionals:
    ${additional.stub()}
    % endfor
}
</%def>

<% data.manual_style_structs = [] %>
<%def name="impl_trait(style_struct_name, skip_longhands='', skip_additionals='')">
<%self:raw_impl_trait style_struct="${next(x for x in data.style_structs if x.name == style_struct_name)}"
                      skip_longhands="${skip_longhands}" skip_additionals="${skip_additionals}">
${caller.body()}
</%self:raw_impl_trait>
<% data.manual_style_structs.append(style_struct_name) %>
</%def>

<%!
class Side(object):
    def __init__(self, name, index):
        self.name = name
        self.ident = name.lower()
        self.index = index

class Corner(object):
    def __init__(self, name, index):
        self.x_name = "NS_CORNER_" + name + "_X"
        self.y_name = "NS_CORNER_" + name + "_Y"
        self.ident = name.lower()
        self.x_index = 2 * index
        self.y_index = 2 * index + 1

SIDES = [Side("Top", 0), Side("Right", 1), Side("Bottom", 2), Side("Left", 3)]
CORNERS = [Corner("TOP_LEFT", 0), Corner("TOP_RIGHT", 1), Corner("BOTTOM_RIGHT", 2), Corner("BOTTOM_LEFT", 3)]
%>

#[allow(dead_code)]
fn static_assert() {
    unsafe {
        % for corner in CORNERS:
        transmute::<_, [u32; ${corner.x_index}]>([1; structs::${corner.x_name} as usize]);
        transmute::<_, [u32; ${corner.y_index}]>([1; structs::${corner.y_name} as usize]);
        % endfor
    }
    // Note: using the above technique with an enum hits a rust bug when |structs| is in a different crate.
    % for side in SIDES:
    { const DETAIL: u32 = [0][(structs::Side::eSide${side.name} as usize != ${side.index}) as usize]; let _ = DETAIL; }
    % endfor
}


<% border_style_keyword = Keyword("border-style",
                                  "none solid double dotted dashed hidden groove ridge inset outset") %>

<% skip_border_longhands = " ".join(["border-{0}-{1}".format(x.ident, y)
                                     for x in SIDES
                                     for y in ["color", "style", "width"]] +
                                    ["border-{0}-radius".format(x.ident.replace("_", "-"))
                                     for x in CORNERS]) %>
<%self:impl_trait style_struct_name="Border"
                  skip_longhands="${skip_border_longhands}"
                  skip_additionals="*">

    % for side in SIDES:
    <% impl_keyword("border_%s_style" % side.ident, "mBorderStyle[%s]" % side.index, border_style_keyword,
                    need_clone=True) %>

    <% impl_color("border_%s_color" % side.ident, "mBorderColor[%s]" % side.index,
                  color_flags_ffi_name="mBorderStyle[%s]" % side.index, need_clone=True) %>

    <% impl_app_units("border_%s_width" % side.ident, "mComputedBorder.%s" % side.ident, need_clone=True,
                      round_to_pixels=True) %>

    pub fn border_${side.ident}_has_nonzero_width(&self) -> bool {
        self.gecko.mComputedBorder.${side.ident} != 0
    }
    % endfor

    % for corner in CORNERS:
    <% impl_corner_style_coord("border_%s_radius" % corner.ident,
                               "mBorderRadius",
                               corner.x_index,
                               corner.y_index,
                               need_clone=True) %>
    % endfor
</%self:impl_trait>

<% skip_margin_longhands = " ".join(["margin-%s" % x.ident for x in SIDES]) %>
<%self:impl_trait style_struct_name="Margin"
                  skip_longhands="${skip_margin_longhands}">

    % for side in SIDES:
    <% impl_split_style_coord("margin_%s" % side.ident,
                              "mMargin",
                              side.index,
                              need_clone=True) %>
    % endfor
</%self:impl_trait>

<% skip_padding_longhands = " ".join(["padding-%s" % x.ident for x in SIDES]) %>
<%self:impl_trait style_struct_name="Padding"
                  skip_longhands="${skip_padding_longhands}">

    % for side in SIDES:
    <% impl_split_style_coord("padding_%s" % side.ident,
                              "mPadding",
                              side.index,
                              need_clone=True) %>
    % endfor
</%self:impl_trait>

<% skip_position_longhands = " ".join(x.ident for x in SIDES) %>
<%self:impl_trait style_struct_name="Position"
                  skip_longhands="${skip_position_longhands} z-index box-sizing">

    % for side in SIDES:
    <% impl_split_style_coord("%s" % side.ident,
                              "mOffset",
                              side.index,
                              need_clone=True) %>
    % endfor

    pub fn set_z_index(&mut self, v: longhands::z_index::computed_value::T) {
        use properties::longhands::z_index::computed_value::T;
        match v {
            T::Auto => self.gecko.mZIndex.set_value(CoordDataValue::Auto),
            T::Number(n) => self.gecko.mZIndex.set_value(CoordDataValue::Integer(n)),
        }
    }

    pub fn copy_z_index_from(&mut self, other: &Self) {
        use gecko_bindings::structs::nsStyleUnit;
        // z-index is never a calc(). If it were, we'd be leaking here, so
        // assert that it isn't.
        debug_assert!(self.gecko.mZIndex.unit() != nsStyleUnit::eStyleUnit_Calc);
        unsafe {
            self.gecko.mZIndex.copy_from_unchecked(&other.gecko.mZIndex);
        }
    }

    pub fn clone_z_index(&self) -> longhands::z_index::computed_value::T {
        use properties::longhands::z_index::computed_value::T;
        return match self.gecko.mZIndex.as_value() {
            CoordDataValue::Auto => T::Auto,
            CoordDataValue::Integer(n) => T::Number(n),
            _ => {
                debug_assert!(false);
                T::Number(0)
            }
        }
    }

    pub fn set_box_sizing(&mut self, v: longhands::box_sizing::computed_value::T) {
        use computed_values::box_sizing::T;
        use gecko_bindings::structs::StyleBoxSizing;
        // TODO: guess what to do with box-sizing: padding-box
        self.gecko.mBoxSizing = match v {
            T::content_box => StyleBoxSizing::Content,
            T::border_box => StyleBoxSizing::Border
        }
    }
    ${impl_simple_copy('box_sizing', 'mBoxSizing')}

</%self:impl_trait>

<% skip_outline_longhands = " ".join("outline-color outline-style outline-width".split() +
                                     ["-moz-outline-radius-{0}".format(x.ident.replace("_", ""))
                                      for x in CORNERS]) %>
<%self:impl_trait style_struct_name="Outline"
                  skip_longhands="${skip_outline_longhands}"
                  skip_additionals="*">

    <% impl_keyword("outline_style", "mOutlineStyle", border_style_keyword, need_clone=True) %>

    <% impl_color("outline_color", "mOutlineColor", color_flags_ffi_name="mOutlineStyle", need_clone=True) %>

    <% impl_app_units("outline_width", "mActualOutlineWidth", need_clone=True,
                      round_to_pixels=True) %>

    % for corner in CORNERS:
    <% impl_corner_style_coord("_moz_outline_radius_%s" % corner.ident.replace("_", ""),
                               "mOutlineRadius",
                               corner.x_index,
                               corner.y_index) %>
    % endfor

    pub fn outline_has_nonzero_width(&self) -> bool {
        self.gecko.mActualOutlineWidth != 0
    }
</%self:impl_trait>

<%self:impl_trait style_struct_name="Font"
    skip_longhands="font-family font-style font-size font-weight"
    skip_additionals="*">

    pub fn set_font_family(&mut self, v: longhands::font_family::computed_value::T) {
        use properties::longhands::font_family::computed_value::FontFamily;
        use gecko_bindings::structs::FontFamilyType;

        let list = &mut self.gecko.mFont.fontlist;
        unsafe { Gecko_FontFamilyList_Clear(list); }

        for family in &v.0 {
            match *family {
                FontFamily::FamilyName(ref name) => {
                    unsafe { Gecko_FontFamilyList_AppendNamed(list, name.as_ptr()); }
                }
                FontFamily::Generic(ref name) => {
                    let family_type =
                        if name == &atom!("serif") { FontFamilyType::eFamily_serif }
                        else if name == &atom!("sans-serif") { FontFamilyType::eFamily_sans_serif }
                        else if name == &atom!("cursive") { FontFamilyType::eFamily_cursive }
                        else if name == &atom!("fantasy") { FontFamilyType::eFamily_fantasy }
                        else if name == &atom!("monospace") { FontFamilyType::eFamily_monospace }
                        else { panic!("Unknown generic font family") };
                    unsafe { Gecko_FontFamilyList_AppendGeneric(list, family_type); }
                }
            }
        }
    }

    pub fn copy_font_family_from(&mut self, other: &Self) {
        unsafe { Gecko_CopyFontFamilyFrom(&mut self.gecko.mFont, &other.gecko.mFont); }
    }

    <%call expr="impl_keyword('font_style', 'mFont.style',
        data.longhands_by_name['font-style'].keyword, need_clone=False)"></%call>

    // FIXME(bholley): Gecko has two different sizes, one of which (mSize) is the
    // actual computed size, and the other of which (mFont.size) is the 'display
    // size' which takes font zooming into account. We don't handle font zooming yet.
    pub fn set_font_size(&mut self, v: longhands::font_size::computed_value::T) {
        self.gecko.mFont.size = v.0;
        self.gecko.mSize = v.0;
    }
    pub fn copy_font_size_from(&mut self, other: &Self) {
        self.gecko.mFont.size = other.gecko.mFont.size;
        self.gecko.mSize = other.gecko.mSize;
    }
    pub fn clone_font_size(&self) -> longhands::font_size::computed_value::T {
        Au(self.gecko.mSize)
    }

    pub fn set_font_weight(&mut self, v: longhands::font_weight::computed_value::T) {
        self.gecko.mFont.weight = v as u16;
    }
    ${impl_simple_copy('font_weight', 'mFont.weight')}

    pub fn clone_font_weight(&self) -> longhands::font_weight::computed_value::T {
        debug_assert!(self.gecko.mFont.weight >= 100);
        debug_assert!(self.gecko.mFont.weight <= 900);
        debug_assert!(self.gecko.mFont.weight % 10 == 0);
        unsafe { transmute(self.gecko.mFont.weight) }
    }

    // This is used for PartialEq, which we don't implement for gecko style structs.
    pub fn compute_font_hash(&mut self) {}

</%self:impl_trait>

<% skip_box_longhands= """display overflow-y vertical-align
                          -moz-binding page-break-before page-break-after""" %>
<%self:impl_trait style_struct_name="Box" skip_longhands="${skip_box_longhands}">

    // We manually-implement the |display| property until we get general
    // infrastructure for preffing certain values.
    <% display_keyword = Keyword("display", "inline block inline-block table inline-table table-row-group " +
                                            "table-header-group table-footer-group table-row table-column-group " +
                                            "table-column table-cell table-caption list-item flex none " +
                                            "-moz-box -moz-inline-box") %>
    ${impl_keyword('display', 'mDisplay', display_keyword, True)}

    // overflow-y is implemented as a newtype of overflow-x, so we need special handling.
    // We could generalize this if we run into other newtype keywords.
    <% overflow_x = data.longhands_by_name["overflow-x"] %>
    pub fn set_overflow_y(&mut self, v: longhands::overflow_y::computed_value::T) {
        use properties::longhands::overflow_x::computed_value::T as BaseType;
        // FIXME(bholley): Align binary representations and ditch |match| for cast + static_asserts
        self.gecko.mOverflowY = match v.0 {
            % for value in overflow_x.keyword.values_for('gecko'):
                BaseType::${to_rust_ident(value)} => structs::${overflow_x.keyword.gecko_constant(value)} as u8,
            % endfor
        };
    }
    ${impl_simple_copy('overflow_y', 'mOverflowY')}
    pub fn clone_overflow_y(&self) -> longhands::overflow_y::computed_value::T {
        use properties::longhands::overflow_x::computed_value::T as BaseType;
        use properties::longhands::overflow_y::computed_value::T as NewType;
        // FIXME(bholley): Align binary representations and ditch |match| for cast + static_asserts
        match self.gecko.mOverflowY as u32 {
            % for value in overflow_x.keyword.values_for('gecko'):
            structs::${overflow_x.keyword.gecko_constant(value)} => NewType(BaseType::${to_rust_ident(value)}),
            % endfor
            x => panic!("Found unexpected value in style struct for overflow_y property: {}", x),
        }
    }

    pub fn set_vertical_align(&mut self, v: longhands::vertical_align::computed_value::T) {
        <% keyword = data.longhands_by_name["vertical-align"].keyword %>
        use properties::longhands::vertical_align::computed_value::T;
        // FIXME: Align binary representations and ditch |match| for cast + static_asserts
        match v {
            % for value in keyword.values_for('gecko'):
                T::${to_rust_ident(value)} =>
                    self.gecko.mVerticalAlign.set_value(
                            CoordDataValue::Enumerated(structs::${keyword.gecko_constant(value)})),
            % endfor
            T::LengthOrPercentage(v) => self.gecko.mVerticalAlign.set(v),
        }
    }

    pub fn clone_vertical_align(&self) -> longhands::vertical_align::computed_value::T {
        use properties::longhands::vertical_align::computed_value::T;
        use values::computed::LengthOrPercentage;

        match self.gecko.mVerticalAlign.as_value() {
            % for value in keyword.values_for('gecko'):
                CoordDataValue::Enumerated(structs::${keyword.gecko_constant(value)}) => T::${to_rust_ident(value)},
            % endfor
                CoordDataValue::Enumerated(_) => panic!("Unexpected enum variant for vertical-align"),
                _ => {
                    let v = LengthOrPercentage::from_gecko_style_coord(&self.gecko.mVerticalAlign)
                        .expect("Expected length or percentage for vertical-align");
                    T::LengthOrPercentage(v)
                }
        }
    }

    <%call expr="impl_coord_copy('vertical_align', 'mVerticalAlign')"></%call>

    #[allow(non_snake_case)]
    pub fn set__moz_binding(&mut self, v: longhands::_moz_binding::computed_value::T) {
        use properties::longhands::_moz_binding::SpecifiedValue as BindingValue;
        match v {
            BindingValue::None => debug_assert!(self.gecko.mBinding.mRawPtr.is_null()),
            BindingValue::Url(ref url, ref extra_data) => {
                unsafe {
                    Gecko_SetMozBinding(&mut self.gecko,
                                        url.as_str().as_ptr(),
                                        url.as_str().len() as u32,
                                        extra_data.base.as_raw(),
                                        extra_data.referrer.as_raw(),
                                        extra_data.principal.as_raw());
                }
            }
        }
    }
    #[allow(non_snake_case)]
    pub fn copy__moz_binding_from(&mut self, other: &Self) {
        unsafe { Gecko_CopyMozBindingFrom(&mut self.gecko, &other.gecko); }
    }

    // Temp fix for Bugzilla bug 24000.
    // Map 'auto' and 'avoid' to false, and 'always', 'left', and 'right' to true.
    // "A conforming user agent may interpret the values 'left' and 'right'
    // as 'always'." - CSS2.1, section 13.3.1
    pub fn set_page_break_before(&mut self, v: longhands::page_break_before::computed_value::T) {
        use computed_values::page_break_before::T;
        let result = match v {
            T::auto   => false,
            T::always => true,
            T::avoid  => false,
            T::left   => true,
            T::right  => true
        };
        // TODO(shinglyu): Rename Gecko's struct to mPageBreakBefore
        self.gecko.mBreakBefore = result;
    }

    ${impl_simple_copy('page_break_before', 'mBreakBefore')}

    // Temp fix for Bugzilla bug 24000.
    // See set_page_break_before for detail.
    pub fn set_page_break_after(&mut self, v: longhands::page_break_after::computed_value::T) {
        use computed_values::page_break_after::T;
        let result = match v {
            T::auto   => false,
            T::always => true,
            T::avoid  => false,
            T::left   => true,
            T::right  => true
        };
        // TODO(shinglyu): Rename Gecko's struct to mPageBreakBefore
        self.gecko.mBreakBefore = result;
    }

    ${impl_simple_copy('page_break_after', 'mBreakAfter')}

</%self:impl_trait>

// TODO: Gecko accepts lists in most background-related properties. We just use
// the first element (which is the common case), but at some point we want to
// add support for parsing these lists in servo and pushing to nsTArray's.
<% skip_background_longhands = """background-color background-repeat
                                  background-image background-clip
                                  background-origin background-attachment
                                  background-position""" %>
<%self:impl_trait style_struct_name="Background"
                  skip_longhands="${skip_background_longhands}"
                  skip_additionals="*">

    <% impl_color("background_color", "mBackgroundColor", need_clone=True) %>

    pub fn copy_background_repeat_from(&mut self, other: &Self) {
        self.gecko.mImage.mRepeatCount = cmp::min(1, other.gecko.mImage.mRepeatCount);
        self.gecko.mImage.mLayers.mFirstElement.mRepeat =
            other.gecko.mImage.mLayers.mFirstElement.mRepeat;
    }

    pub fn set_background_repeat(&mut self, v: longhands::background_repeat::computed_value::T) {
        use properties::longhands::background_repeat::computed_value::T;
        use gecko_bindings::structs::{NS_STYLE_IMAGELAYER_REPEAT_REPEAT, NS_STYLE_IMAGELAYER_REPEAT_NO_REPEAT};
        use gecko_bindings::structs::nsStyleImageLayers_Repeat;
        let (repeat_x, repeat_y) = match v {
            T::repeat_x => (NS_STYLE_IMAGELAYER_REPEAT_REPEAT,
                            NS_STYLE_IMAGELAYER_REPEAT_NO_REPEAT),
            T::repeat_y => (NS_STYLE_IMAGELAYER_REPEAT_NO_REPEAT,
                            NS_STYLE_IMAGELAYER_REPEAT_REPEAT),
            T::repeat => (NS_STYLE_IMAGELAYER_REPEAT_REPEAT,
                          NS_STYLE_IMAGELAYER_REPEAT_REPEAT),
            T::no_repeat => (NS_STYLE_IMAGELAYER_REPEAT_NO_REPEAT,
                             NS_STYLE_IMAGELAYER_REPEAT_NO_REPEAT),
        };

        self.gecko.mImage.mRepeatCount = 1;
        self.gecko.mImage.mLayers.mFirstElement.mRepeat = nsStyleImageLayers_Repeat {
            mXRepeat: repeat_x as u8,
            mYRepeat: repeat_y as u8,
        };
    }

    pub fn copy_background_clip_from(&mut self, other: &Self) {
        self.gecko.mImage.mClipCount = cmp::min(1, other.gecko.mImage.mClipCount);
        self.gecko.mImage.mLayers.mFirstElement.mClip =
            other.gecko.mImage.mLayers.mFirstElement.mClip;
    }

    pub fn set_background_clip(&mut self, v: longhands::background_clip::computed_value::T) {
        use properties::longhands::background_clip::computed_value::T;
        self.gecko.mImage.mClipCount = 1;

        // TODO: Gecko supports background-clip: text, but just on -webkit-
        // prefixed properties.
        self.gecko.mImage.mLayers.mFirstElement.mClip = match v {
            T::border_box => structs::NS_STYLE_IMAGELAYER_CLIP_BORDER as u8,
            T::padding_box => structs::NS_STYLE_IMAGELAYER_CLIP_PADDING as u8,
            T::content_box => structs::NS_STYLE_IMAGELAYER_CLIP_CONTENT as u8,
        };
    }

    pub fn copy_background_origin_from(&mut self, other: &Self) {
        self.gecko.mImage.mOriginCount = cmp::min(1, other.gecko.mImage.mOriginCount);
        self.gecko.mImage.mLayers.mFirstElement.mOrigin =
            other.gecko.mImage.mLayers.mFirstElement.mOrigin;
    }

    pub fn set_background_origin(&mut self, v: longhands::background_origin::computed_value::T) {
        use properties::longhands::background_origin::computed_value::T;

        self.gecko.mImage.mOriginCount = 1;
        self.gecko.mImage.mLayers.mFirstElement.mOrigin = match v {
            T::border_box => structs::NS_STYLE_IMAGELAYER_ORIGIN_BORDER as u8,
            T::padding_box => structs::NS_STYLE_IMAGELAYER_ORIGIN_PADDING as u8,
            T::content_box => structs::NS_STYLE_IMAGELAYER_ORIGIN_CONTENT as u8,
        };
    }

    pub fn copy_background_attachment_from(&mut self, other: &Self) {
        self.gecko.mImage.mAttachmentCount = cmp::min(1, other.gecko.mImage.mAttachmentCount);
        self.gecko.mImage.mLayers.mFirstElement.mAttachment =
            other.gecko.mImage.mLayers.mFirstElement.mAttachment;
    }

    pub fn set_background_attachment(&mut self, v: longhands::background_attachment::computed_value::T) {
        use properties::longhands::background_attachment::computed_value::T;

        self.gecko.mImage.mAttachmentCount = 1;
        self.gecko.mImage.mLayers.mFirstElement.mAttachment = match v {
            T::scroll => structs::NS_STYLE_IMAGELAYER_ATTACHMENT_SCROLL as u8,
            T::fixed => structs::NS_STYLE_IMAGELAYER_ATTACHMENT_FIXED as u8,
            T::local => structs::NS_STYLE_IMAGELAYER_ATTACHMENT_LOCAL as u8,
        };
    }

    pub fn copy_background_position_from(&mut self, other: &Self) {
        self.gecko.mImage.mPositionXCount = cmp::min(1, other.gecko.mImage.mPositionXCount);
        self.gecko.mImage.mPositionYCount = cmp::min(1, other.gecko.mImage.mPositionYCount);
        self.gecko.mImage.mLayers.mFirstElement.mPosition =
            other.gecko.mImage.mLayers.mFirstElement.mPosition;
    }

    pub fn clone_background_position(&self) -> longhands::background_position::computed_value::T {
        use values::computed::position::Position;
        let position = &self.gecko.mImage.mLayers.mFirstElement.mPosition;
        longhands::background_position::computed_value::T(Position {
            horizontal: position.mXPosition.into(),
            vertical: position.mYPosition.into(),
        })
    }

    pub fn set_background_position(&mut self, v: longhands::background_position::computed_value::T) {
        let position = &mut self.gecko.mImage.mLayers.mFirstElement.mPosition;
        position.mXPosition = v.0.horizontal.into();
        position.mYPosition = v.0.vertical.into();
        self.gecko.mImage.mPositionXCount = 1;
        self.gecko.mImage.mPositionYCount = 1;
    }

    pub fn copy_background_image_from(&mut self, other: &Self) {
        unsafe {
            Gecko_CopyImageValueFrom(&mut self.gecko.mImage.mLayers.mFirstElement.mImage,
                                     &other.gecko.mImage.mLayers.mFirstElement.mImage);
        }
    }

    pub fn set_background_image(&mut self, images: longhands::background_image::computed_value::T) {
        use gecko_bindings::structs::nsStyleImageLayers_LayerType as LayerType;
        use gecko_bindings::structs::{NS_STYLE_GRADIENT_SHAPE_LINEAR, NS_STYLE_GRADIENT_SIZE_FARTHEST_CORNER};
        use gecko_bindings::structs::nsStyleCoord;
        use values::computed::Image;
        use values::specified::AngleOrCorner;
        use cssparser::Color as CSSColor;

        unsafe {
            // Prevent leaking of the last element we did set
            for image in &mut self.gecko.mImage.mLayers {
                Gecko_SetNullImageValue(&mut image.mImage)
            }
            Gecko_EnsureImageLayersLength(&mut self.gecko.mImage, images.0.len());
            for image in &mut self.gecko.mImage.mLayers {
                Gecko_InitializeImageLayer(image, LayerType::Background);
            }
        }

        self.gecko.mImage.mImageCount = cmp::max(self.gecko.mImage.mLayers.len() as u32,
                                                 self.gecko.mImage.mImageCount);

        // TODO: pre-grow the nsTArray to the right capacity
        // otherwise the below code won't work
        for (image, geckoimage) in images.0.into_iter().zip(self.gecko.mImage.mLayers.iter_mut()) {
            if let Some(image) = image.0 {
                match image {
                    Image::LinearGradient(ref gradient) => {
                        let stop_count = gradient.stops.len();
                        if stop_count >= ::std::u32::MAX as usize {
                            warn!("stylo: Prevented overflow due to too many gradient stops");
                            return;
                        }

                        let gecko_gradient = unsafe {
                            Gecko_CreateGradient(NS_STYLE_GRADIENT_SHAPE_LINEAR as u8,
                                                 NS_STYLE_GRADIENT_SIZE_FARTHEST_CORNER as u8,
                                                 /* repeating = */ false,
                                                 /* legacy_syntax = */ false,
                                                 stop_count as u32)
                        };

                        // TODO: figure out what gecko does in the `corner` case.
                        if let AngleOrCorner::Angle(angle) = gradient.angle_or_corner {
                            unsafe {
                                (*gecko_gradient).mAngle.set(angle);
                            }
                        }

                        let mut coord: nsStyleCoord = unsafe { uninitialized() };
                        for (index, stop) in gradient.stops.iter().enumerate() {
                            // NB: stops are guaranteed to be none in the gecko side by
                            // default.
                            coord.set(stop.position);
                            let color = match stop.color {
                                CSSColor::CurrentColor => {
                                    // TODO(emilio): gecko just stores an nscolor,
                                    // and it doesn't seem to support currentColor
                                    // as value in a gradient.
                                    //
                                    // Double-check it and either remove
                                    // currentColor for servo or see how gecko
                                    // handles this.
                                    0
                                },
                                CSSColor::RGBA(ref rgba) => convert_rgba_to_nscolor(rgba),
                            };

                            let mut stop = unsafe {
                                &mut (*gecko_gradient).mStops[index]
                            };

                            stop.mColor = color;
                            stop.mIsInterpolationHint = false;
                            stop.mLocation.copy_from(&coord);
                        }

                        unsafe {
                            Gecko_SetGradientImageValue(&mut geckoimage.mImage, gecko_gradient);
                        }
                    },
                    Image::Url(..) => {
                        // let utf8_bytes = url.as_bytes();
                        // Gecko_SetUrlImageValue(&mut self.gecko.mImage.mLayers.mFirstElement,
                        //                        utf8_bytes.as_ptr() as *const _,
                        //                        utf8_bytes.len());
                        warn!("stylo: imgRequestProxies are not threadsafe in gecko, \
                               background-image: url() not yet implemented");
                    }
                }
            }

        }
    }
</%self:impl_trait>

<%self:impl_trait style_struct_name="List" skip_longhands="list-style-type" skip_additionals="*">

    ${impl_keyword_setter("list_style_type", "__LIST_STYLE_TYPE__",
                           data.longhands_by_name["list-style-type"].keyword)}
    pub fn copy_list_style_type_from(&mut self, other: &Self) {
        unsafe {
            Gecko_CopyListStyleTypeFrom(&mut self.gecko, &other.gecko);
        }
    }

</%self:impl_trait>

<%self:impl_trait style_struct_name="InheritedText"
                  skip_longhands="text-align line-height word-spacing">

    <% text_align_keyword = Keyword("text-align", "start end left right center justify -moz-center -moz-left " +
                                                  "-moz-right match-parent") %>
    ${impl_keyword('text_align', 'mTextAlign', text_align_keyword, need_clone=False)}

    pub fn set_line_height(&mut self, v: longhands::line_height::computed_value::T) {
        use properties::longhands::line_height::computed_value::T;
        // FIXME: Align binary representations and ditch |match| for cast + static_asserts
        let en = match v {
            T::Normal => CoordDataValue::Normal,
            T::Length(val) => CoordDataValue::Coord(val.0),
            T::Number(val) => CoordDataValue::Factor(val),
            T::MozBlockHeight =>
                    CoordDataValue::Enumerated(structs::NS_STYLE_LINE_HEIGHT_BLOCK_HEIGHT),
        };
        self.gecko.mLineHeight.set_value(en);
    }

    pub fn clone_line_height(&self) -> longhands::line_height::computed_value::T {
        use properties::longhands::line_height::computed_value::T;
        return match self.gecko.mLineHeight.as_value() {
            CoordDataValue::Normal => T::Normal,
            CoordDataValue::Coord(coord) => T::Length(Au(coord)),
            CoordDataValue::Factor(n) => T::Number(n),
            CoordDataValue::Enumerated(val) if val == structs::NS_STYLE_LINE_HEIGHT_BLOCK_HEIGHT =>
                T::MozBlockHeight,
            _ => {
                debug_assert!(false);
                T::MozBlockHeight
            }
        }
    }

    <%call expr="impl_coord_copy('line_height', 'mLineHeight')"></%call>

    pub fn set_word_spacing(&mut self, v: longhands::word_spacing::computed_value::T) {
        use values::computed::LengthOrPercentage::*;

        match v.0 {
            Some(lop) => match lop {
                Length(au) => self.gecko.mWordSpacing.set_value(CoordDataValue::Coord(au.0)),
                Percentage(f) => self.gecko.mWordSpacing.set_value(CoordDataValue::Percent(f)),
                Calc(l_p) => self.gecko.mWordSpacing.set_value(CoordDataValue::Calc(l_p.into())),
            },
            // https://drafts.csswg.org/css-text-3/#valdef-word-spacing-normal
            None => self.gecko.mWordSpacing.set_value(CoordDataValue::Coord(0)),
        }
    }

    <%call expr="impl_coord_copy('word_spacing', 'mWordSpacing')"></%call>

</%self:impl_trait>

<%self:impl_trait style_struct_name="Text"
                  skip_longhands="text-decoration-color text-decoration-line"
                  skip_additionals="*">

    ${impl_color("text_decoration_color", "mTextDecorationColor",
                  color_flags_ffi_name="mTextDecorationStyle", need_clone=True)}

    pub fn set_text_decoration_line(&mut self, v: longhands::text_decoration_line::computed_value::T) {
        let mut bits: u8 = 0;
        if v.underline {
            bits |= structs::NS_STYLE_TEXT_DECORATION_LINE_UNDERLINE as u8;
        }
        if v.overline {
            bits |= structs::NS_STYLE_TEXT_DECORATION_LINE_OVERLINE as u8;
        }
        if v.line_through {
            bits |= structs::NS_STYLE_TEXT_DECORATION_LINE_LINE_THROUGH as u8;
        }
        self.gecko.mTextDecorationLine = bits;
    }

    ${impl_simple_copy('text_decoration_line', 'mTextDecorationLine')}

    #[inline]
    pub fn has_underline(&self) -> bool {
        (self.gecko.mTextDecorationLine & (structs::NS_STYLE_TEXT_DECORATION_LINE_UNDERLINE as u8)) != 0
    }

    #[inline]
    pub fn has_overline(&self) -> bool {
        (self.gecko.mTextDecorationLine & (structs::NS_STYLE_TEXT_DECORATION_LINE_OVERLINE as u8)) != 0
    }

    #[inline]
    pub fn has_line_through(&self) -> bool {
        (self.gecko.mTextDecorationLine & (structs::NS_STYLE_TEXT_DECORATION_LINE_LINE_THROUGH as u8)) != 0
    }
</%self:impl_trait>

<%self:impl_trait style_struct_name="SVG"
                  skip_longhands="flood-color lighting-color stop-color"
                  skip_additionals="*">

    <% impl_color("flood_color", "mFloodColor") %>

    <% impl_color("lighting_color", "mLightingColor") %>

    <% impl_color("stop_color", "mStopColor") %>

</%self:impl_trait>

<%self:impl_trait style_struct_name="Color"
                  skip_longhands="*">
    pub fn set_color(&mut self, v: longhands::color::computed_value::T) {
        let result = convert_rgba_to_nscolor(&v);
        ${set_gecko_property("mColor", "result")}
    }

    <%call expr="impl_simple_copy('color', 'mColor')"></%call>

    pub fn clone_color(&self) -> longhands::color::computed_value::T {
        let color = ${get_gecko_property("mColor")} as u32;
        convert_nscolor_to_rgba(color)
    }
</%self:impl_trait>

<%self:impl_trait style_struct_name="Pointing"
                  skip_longhands="cursor">
    pub fn set_cursor(&mut self, v: longhands::cursor::computed_value::T) {
        use properties::longhands::cursor::computed_value::T;
        use style_traits::cursor::Cursor;

        self.gecko.mCursor = match v {
            T::AutoCursor => structs::NS_STYLE_CURSOR_AUTO,
            T::SpecifiedCursor(cursor) => match cursor {
                Cursor::None => structs::NS_STYLE_CURSOR_NONE,
                Cursor::Default => structs::NS_STYLE_CURSOR_DEFAULT,
                Cursor::Pointer => structs::NS_STYLE_CURSOR_POINTER,
                Cursor::ContextMenu => structs::NS_STYLE_CURSOR_CONTEXT_MENU,
                Cursor::Help => structs::NS_STYLE_CURSOR_HELP,
                Cursor::Progress => structs::NS_STYLE_CURSOR_DEFAULT, // Gecko doesn't support "progress" yet
                Cursor::Wait => structs::NS_STYLE_CURSOR_WAIT,
                Cursor::Cell => structs::NS_STYLE_CURSOR_CELL,
                Cursor::Crosshair => structs::NS_STYLE_CURSOR_CROSSHAIR,
                Cursor::Text => structs::NS_STYLE_CURSOR_TEXT,
                Cursor::VerticalText => structs::NS_STYLE_CURSOR_VERTICAL_TEXT,
                Cursor::Alias => structs::NS_STYLE_CURSOR_ALIAS,
                Cursor::Copy => structs::NS_STYLE_CURSOR_COPY,
                Cursor::Move => structs::NS_STYLE_CURSOR_MOVE,
                Cursor::NoDrop => structs::NS_STYLE_CURSOR_NO_DROP,
                Cursor::NotAllowed => structs::NS_STYLE_CURSOR_NOT_ALLOWED,
                Cursor::Grab => structs::NS_STYLE_CURSOR_GRAB,
                Cursor::Grabbing => structs::NS_STYLE_CURSOR_GRABBING,
                Cursor::EResize => structs::NS_STYLE_CURSOR_E_RESIZE,
                Cursor::NResize => structs::NS_STYLE_CURSOR_N_RESIZE,
                Cursor::NeResize => structs::NS_STYLE_CURSOR_NE_RESIZE,
                Cursor::NwResize => structs::NS_STYLE_CURSOR_NW_RESIZE,
                Cursor::SResize => structs::NS_STYLE_CURSOR_S_RESIZE,
                Cursor::SeResize => structs::NS_STYLE_CURSOR_SE_RESIZE,
                Cursor::SwResize => structs::NS_STYLE_CURSOR_SW_RESIZE,
                Cursor::WResize => structs::NS_STYLE_CURSOR_W_RESIZE,
                Cursor::EwResize => structs::NS_STYLE_CURSOR_EW_RESIZE,
                Cursor::NsResize => structs::NS_STYLE_CURSOR_NS_RESIZE,
                Cursor::NeswResize => structs::NS_STYLE_CURSOR_NESW_RESIZE,
                Cursor::NwseResize => structs::NS_STYLE_CURSOR_NWSE_RESIZE,
                Cursor::ColResize => structs::NS_STYLE_CURSOR_COL_RESIZE,
                Cursor::RowResize => structs::NS_STYLE_CURSOR_ROW_RESIZE,
                Cursor::AllScroll => structs::NS_STYLE_CURSOR_ALL_SCROLL,
                Cursor::ZoomIn => structs::NS_STYLE_CURSOR_ZOOM_IN,
                Cursor::ZoomOut => structs::NS_STYLE_CURSOR_ZOOM_OUT,
            }
        } as u8;
    }

    ${impl_simple_copy('cursor', 'mCursor')}
</%self:impl_trait>

<%self:impl_trait style_struct_name="Column"
                  skip_longhands="column-width">

    pub fn set_column_width(&mut self, v: longhands::column_width::computed_value::T) {
        match v.0 {
            Some(au) => self.gecko.mColumnWidth.set_value(CoordDataValue::Coord(au.0)),
            None => self.gecko.mColumnWidth.set_value(CoordDataValue::Auto),
        }
    }

    ${impl_coord_copy('column_width', 'mColumnWidth')}
</%self:impl_trait>

<%def name="define_ffi_struct_accessor(style_struct)">
#[no_mangle]
#[allow(non_snake_case, unused_variables)]
pub extern "C" fn Servo_GetStyle${style_struct.gecko_name}(computed_values: *mut bindings::ServoComputedValues)
  -> *const ${style_struct.gecko_ffi_name} {
    type Helpers = ArcHelpers<bindings::ServoComputedValues, ComputedValues>;
    Helpers::with(computed_values, |values| values.get_${style_struct.name_lower}().get_gecko()
                                                as *const ${style_struct.gecko_ffi_name})
}
</%def>

% for style_struct in data.style_structs:
${declare_style_struct(style_struct)}
${impl_style_struct(style_struct)}
% if not style_struct.name in data.manual_style_structs:
<%self:raw_impl_trait style_struct="${style_struct}"></%self:raw_impl_trait>
% endif
${define_ffi_struct_accessor(style_struct)}
% endfor

// To avoid UB, we store the initial values as a atomic. It would be nice to
// store them as AtomicPtr, but we can't have static AtomicPtr without const
// fns, which aren't in stable Rust.
static INITIAL_VALUES_STORAGE: AtomicUsize = ATOMIC_USIZE_INIT;
unsafe fn raw_initial_values() -> *mut ComputedValues {
    INITIAL_VALUES_STORAGE.load(Ordering::Relaxed) as *mut ComputedValues
}
unsafe fn set_raw_initial_values(v: *mut ComputedValues) {
    INITIAL_VALUES_STORAGE.store(v as usize, Ordering::Relaxed);
}

static CASCADE_PROPERTY: [CascadePropertyFn; ${len(data.longhands)}] = [
    % for property in data.longhands:
        longhands::${property.ident}::cascade_property,
    % endfor
];
