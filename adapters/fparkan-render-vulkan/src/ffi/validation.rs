#![allow(unsafe_code)]

use ash::vk;
use std::collections::BTreeSet;
use std::ffi::CStr;
use std::sync::{
    atomic::{AtomicU32, Ordering},
    Mutex,
};

use super::{VulkanInstanceProbe, VulkanSmokeRendererError, VulkanValidationReport};

struct VulkanValidationShared {
    warning_count: AtomicU32,
    error_count: AtomicU32,
    vuids: Mutex<BTreeSet<String>>,
}

impl Default for VulkanValidationShared {
    fn default() -> Self {
        Self {
            warning_count: AtomicU32::new(0),
            error_count: AtomicU32::new(0),
            vuids: Mutex::new(BTreeSet::new()),
        }
    }
}

pub(super) struct VulkanValidationMessenger {
    loader: ash::ext::debug_utils::Instance,
    messenger: vk::DebugUtilsMessengerEXT,
    shared: Box<VulkanValidationShared>,
}

impl VulkanValidationMessenger {
    pub(super) fn report(&self) -> VulkanValidationReport {
        let vuids = self
            .shared
            .vuids
            .lock()
            .map(|values| values.iter().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        VulkanValidationReport {
            warning_count: self.shared.warning_count.load(Ordering::Relaxed),
            error_count: self.shared.error_count.load(Ordering::Relaxed),
            vuids,
        }
    }
}

impl Drop for VulkanValidationMessenger {
    fn drop(&mut self) {
        // SAFETY: The messenger belongs to this instance-level loader and is destroyed once.
        unsafe {
            self.loader
                .destroy_debug_utils_messenger(self.messenger, None);
        };
    }
}

unsafe extern "system" fn vulkan_validation_callback(
    message_severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    _message_types: vk::DebugUtilsMessageTypeFlagsEXT,
    callback_data: *const vk::DebugUtilsMessengerCallbackDataEXT<'_>,
    user_data: *mut std::ffi::c_void,
) -> vk::Bool32 {
    // SAFETY: The debug messenger stores a stable pointer to `VulkanValidationShared` for the messenger lifetime.
    let Some(shared) = (unsafe { (user_data as *const VulkanValidationShared).as_ref() }) else {
        return vk::FALSE;
    };
    if message_severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::ERROR) {
        shared.error_count.fetch_add(1, Ordering::Relaxed);
    } else if message_severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::WARNING) {
        shared.warning_count.fetch_add(1, Ordering::Relaxed);
    }
    // SAFETY: Vulkan invokes the callback with either a null pointer or a valid callback-data payload.
    let Some(callback_data) = (unsafe { callback_data.as_ref() }) else {
        return vk::FALSE;
    };
    if let Some(vuid) = (!callback_data.p_message_id_name.is_null()).then(|| {
        // SAFETY: `p_message_id_name` is a Vulkan-owned NUL-terminated string for the callback duration.
        unsafe { CStr::from_ptr(callback_data.p_message_id_name) }
            .to_string_lossy()
            .into_owned()
    }) {
        if vuid.starts_with("VUID-") {
            if let Ok(mut vuids) = shared.vuids.lock() {
                vuids.insert(vuid);
            }
        }
    }
    vk::FALSE
}

pub(super) fn create_validation_messenger(
    instance: &VulkanInstanceProbe,
) -> Result<VulkanValidationMessenger, VulkanSmokeRendererError> {
    let shared = Box::new(VulkanValidationShared::default());
    let loader = ash::ext::debug_utils::Instance::new(&instance.entry, &instance.instance);
    let create_info = vk::DebugUtilsMessengerCreateInfoEXT::default()
        .message_severity(
            vk::DebugUtilsMessageSeverityFlagsEXT::WARNING
                | vk::DebugUtilsMessageSeverityFlagsEXT::ERROR,
        )
        .message_type(
            vk::DebugUtilsMessageTypeFlagsEXT::GENERAL
                | vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION
                | vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE,
        )
        .pfn_user_callback(Some(vulkan_validation_callback))
        .user_data((&raw const *shared).cast_mut().cast());
    let messenger =
        // SAFETY: The create info points at a stable boxed user-data allocation for the messenger lifetime.
        unsafe { loader.create_debug_utils_messenger(&create_info, None) }.map_err(|error| {
            VulkanSmokeRendererError::VulkanOperation {
                context: "vkCreateDebugUtilsMessengerEXT",
                result: error,
            }
        })?;
    Ok(VulkanValidationMessenger {
        loader,
        messenger,
        shared,
    })
}
