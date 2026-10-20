@@
     pub fn attach_to_device(self: Arc<Self>, device: Arc<Device>, owned: bool) -> Result<Arc<DeviceSpecificAllocation>, NtStatus> {
         let mut attached = self.device_specific.lock();
         let context_id = device.context().ok_or(STATUS::REINITIALIZATION_NEEDED).inspect_err(|e|
             error!("{}: context on device {:?} was not initialized yet", function!(), device)
         )?;
@@
         if let Some(device_specific) = owned_device_specific.iter().find(|ds| ds.device.upgrade().and_then(|dev| Some(Arc::ptr_eq(&dev, &device))).unwrap_or(false)) {
-            assert!(Arc::ptr_eq(&device_specific.alloc.upgrade().unwrap(), &alloc));
+            let existing_alloc = match device_specific.alloc.upgrade() {
+                Some(existing) => existing,
+                None => {
+                    error!("{}: stale device-specific allocation for resource {} while attaching to {:?}", function!(), alloc.id().unwrap_or(NonZero::new(0).unwrap()), device);
+                    return Err(NtStatus(STATUS::INVALID_HANDLE));
+                }
+            };
+
+            if !Arc::ptr_eq(&existing_alloc, &alloc) {
+                warn!("{}: device-specific allocation for resource {} points at a different allocation; ignoring duplicate attach", function!(), alloc.id().unwrap_or(NonZero::new(0).unwrap()));
+                return Ok(());
+            }
             /* Already attached, no need to do anything */
         } else {
             /* Attaching once more */
             let device_specific = alloc.attach_to_device(device, false)?;
             owned_device_specific.push(device_specific);
@@
     pub fn attach_pages(&self, offset: u64, pages: &[DXGK_PTE]) -> bool {
         match &self.resource {
             VirtioResource::Sysmem { .. } => false,
             VirtioResource::_3D { map, size, .. } => {
                 let mut map = map.write();
                 let mut attached_size = map.1.iter().fold(0u64, |len, entry| len + entry.length as u64);
                 if attached_size >= *size {
@@
                 for entry in pages {
                     if !entry.Flags().Valid() {
                         error!("{}: detaching is not yet supported: {:?}", function!(), pages);
                         return false;
                     }
                 }
 
                 let entries = pages.iter().map(|entry| {
                     let length = entry.Len() as u32;
-                    assert!(length > 0, "unexpected page size: {}", length);
-                    assert!(entry.Flags().Segment() == 0, "only system memory backing for 3d resources is supported, not {}", entry.Flags().Segment());
+                    if length == 0 {
+                        error!("{}: unexpected zero-length page entry while attaching resource {}: {:?}", function!(), self.id, pages);
+                        return Err(())
+                    }
+                    if entry.Flags().Segment() != 0 {
+                        error!("{}: unsupported non-system-memory backing segment {} while attaching resource {}", function!(), entry.Flags().Segment(), self.id);
+                        return Err(())
+                    }
                     let addr = entry.PageAddress() * wdk::wdm::PAGE_SIZE as u64;
 
                     attached_size += length as u64;
 
                     MemEntry {
@@
-                }).coalesce(|previous, current| {
+                    Ok(entry)
+                }).collect::<Result<Vec<_>, ()>>();
+
+                let entries = match entries {
+                    Ok(entries) => entries,
+                    Err(()) => return false,
+                };
+
+                let entries = entries.into_iter().coalesce(|previous, current| {
                     if let Some(new_length) = merge_entries(&previous, &current) {
                         // Physically contiguous pages, reuse last entry
                         Ok(MemEntry {
                             length: new_length,
                             ..previous
@@
                         }
                     }
                 });
 
                 if offset < map.0 {
                     warn!("{}: out of order attach: attached pages start from offset {}, new offset {}", function!(), offset, map.0);
                     map.1.insert_many(0, entries);
                 } else {
                     for entry in entries {
                         if let Some(previous) = map.1.last_mut() && let Some(new_length) = merge_entries(previous, &entry) {
                             // Physically contiguous pages, reuse last entry
                             previous.length = new_length;
                         } else {
                             map.1.push(entry);
                         }
                     }
                 }
@@
                 debug!("{}: ready: {}, pages: {:?}", function!(), attached_size >= *size, pages);
                 debug!("{}: ({} / {}): {:?}", function!(), map.0, offset, map.1);
                 map.0 = offset;
 
                 attached_size >= *size
             },
             VirtioResource::Blob { .. } => false,
         }
     }
