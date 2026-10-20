impl Device {
    fn teardown_contexts(&self) {
        // The queued UNREF retains the backing through its response, including
        // setup failure and timeout paths. Host UNREF detaches all contexts.
        if let Some(query) = self.query_buffer.get_mut().take() {
            drop(query);
        }

        if let Some(main) = self.main_context.get_mut().take() {
            debug!("Destroying context {:?}", main);

            let _ = self.chan.context_destroy(main.id).inspect_err(|e|
                error!("{}: failed to destroy context: {:?}", function!(), e)
            );
        }

        if let Some(mut blit) = self.virgl_blit_context.get_mut().take() {
            if let Some(blit_ctx) = blit.context.take() {
                debug!("Destroying context {:?}", blit_ctx);

                let _ = self.chan.context_destroy(blit_ctx.id).inspect_err(|e|
                    error!("{}: failed to destroy context: {:?}", function!(), e)
                );
            }
        }
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        self.tag = 0;
        self.teardown_contexts();
    }
}
