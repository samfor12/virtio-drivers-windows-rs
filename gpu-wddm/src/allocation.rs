    pub fn attach_pages(&self, offset: u64, pages: &[DXGK_PTE]) -> bool {
        match &self.resource {
            VirtioResource::Sysmem { .. } => false,
            VirtioResource::_3D { map, size, .. } => {
                let mut map = map.write();
                let mut attached_size = map.1.iter().fold(0u64, |len, entry| len + entry.length as u64);
                if attached_size >= *size {
                    // We got a duplicate, probably because there wasn't enough DMA buffer space to encode the command last time
                    // TODO: this should be detected more reliably by checking the virtual address
                    warn!("{}: already fully attached, but tried to attach again: {:?}", function!(), pages);
                    return true;
                }

                if attached_size > 0 && offset == 0 {
                    warn!("{}: already fully attached, but tried to attach again from the same offset: {:?}", function!(), pages);
                }

                for entry in pages {
                    if !entry.Flags().Valid() {
                        error!("{}: invalid page entry while attaching resource {}: {:?}", function!(), self.id, pages);
                        return false;
                    }
                }

                let entries = match pages.iter().map(|entry| {
                    let length = entry.Len() as u32;
                    if length == 0 {
                        error!("{}: unexpected zero-length page size for resource {}", function!(), self.id);
                        return Err(());
                    }
                    if entry.Flags().Segment() != 0 {
                        error!("{}: unsupported non-system-memory backing segment {} while attaching resource {}", function!(), entry.Flags().Segment(), self.id);
                        return Err(());
                    }

                    let addr = entry.PageAddress() * wdk::wdm::PAGE_SIZE as u64;
                    attached_size += length as u64;

                    Ok(MemEntry {
                        addr,
                        length,
                        _padding: 0,
                    })
                }).collect::<Result<Vec<_>, ()>>() {
                    Ok(entries) => entries,
                    Err(()) => return false,
                };

                let entries = entries.into_iter().coalesce(|previous, current| {
                    if let Some(new_length) = merge_entries(&previous, &current) {
                        // Physically contiguous pages, reuse last entry
                        Ok(MemEntry {
                            length: new_length,
                            ..previous
                        })
                    } else {
                        Err((previous, current))
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

                debug!("{}: ready: {}, pages: {:?}", function!(), attached_size >= *size, pages);
                debug!("{}: ({} / {}): {:?}", function!(), map.0, offset, map.1);
                map.0 = offset;

                attached_size >= *size
            },
            VirtioResource::Blob { .. } => false,
        }
    }

    pub fn query_layout(self: &Arc<Allocation>, device: Arc<Device>) -> Result<Option<VirglResourceLayout>, NtStatus> {
        match &self.resource {
            VirtioResource::_3D { layout, .. } => {
                if let Some(layout) = *layout.read() {
                    return Ok(Some(layout))
                }
            }
            _ => {
                error!("{}: cannot query layout for blob resource: {:?}", function!(), self);
                return Err(NtStatus(STATUS::INVALID_PARAMETER));
            }
        }

        match device.query_layout(&self) {
            Ok(layout_) => {
                match &self.resource {
                    VirtioResource::_3D { layout, .. } => {
                        layout.write().replace(layout_);
                    }
                    _ => unreachable!(),
                }

                Ok(Some(layout_))
            },
            Err(NtStatus(STATUS::REINITIALIZATION_NEEDED)) => Ok(None),
            Err(e) => {
                error!("{}: failed to query layout: {:?}", function!(), e);
                Err(e)
            }
        }
    }
}

impl Drop for Allocation {
    fn drop(&mut self) {
        self.tag = 0;
        if let Some(KernelMap(mdl, ptr)) = self.kernel_map.get_mut().take() {
            mm_unmap_locked_pages(&mdl, ptr);
            drop(mdl);
        }
    }
}

#[repr(C)]
#[derive(Tagged, Debug)]
#[tagged(VIRTIO_GPU_RESOURCE_TAG)]
pub struct Resource {
    pub tag: u64,
    alloc: Weak<Allocation>,
    owned_device_specific: SpinMutex<SmallVec<[Arc<DeviceSpecificAllocation>; 2]>>
}

impl Resource {
    pub fn new(alloc: &Arc<Allocation>) -> Result<Arc<Self>, NtStatus> {
        Ok(Arc::try_new(Self {
            tag: VIRTIO_GPU_RESOURCE_TAG,
            alloc: Arc::downgrade(alloc),
            owned_device_specific: SpinMutex::new(SmallVec::new()),
        })?)
    }

    pub fn attach_to_device(self: Arc<Self>, device: Arc<Device>) -> Result<(), NtStatus> {
        let Some(alloc) = self.alloc.upgrade() else {
            error!("{}: dead allocation for resource: {:?}", function!(), self);
            return Err(NtStatus(STATUS::INVALID_PARAMETER));
        };

        warn!("{}: alloc: ({}, {:?}), dev: {:?}", function!(), alloc.id().unwrap(), alloc.resource(), device);

        let mut owned_device_specific = self.owned_device_specific.lock();

        if let Some(device_specific) = owned_device_specific.iter().find(|ds| ds.device.upgrade().and_then(|dev| Some(Arc::ptr_eq(&dev, &device))).unwrap_or(false)) {
            let existing = match device_specific.alloc.upgrade() {
                Some(existing) => existing,
                None => {
                    warn!("{}: stale weak reference during duplicate attach for resource {}", function!(), alloc.id().unwrap_or(NonZero::new(1).unwrap()));
                    return Ok(());
                }
            };

            if !Arc::ptr_eq(&existing, &alloc) {
                warn!("{}: duplicate attach mismatch for resource {}; ignoring", function!(), alloc.id().unwrap_or(NonZero::new(1).unwrap()));
                return Ok(());
            }

            /* Already attached, no need to do anything */
            return Ok(());
        }

        /* Attaching once more */
        let device_specific = alloc.attach_to_device(device, false)?;
        owned_device_specific.push(device_specific);

        Ok(())
    }
}
