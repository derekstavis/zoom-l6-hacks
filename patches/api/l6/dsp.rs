// L6 MAIN 1.10 dsp bindings.

firmware_symbols! {
    pub sai1_rx_dma_callback at 0x8003_35f8;
    pub audio_input_block_semaphore_callback at 0x8001_80f8;
    pub audio_staging_ring_producer at 0x8001_2388;
    pub audio_process_block_consumer at 0x8001_0698;
    pub dsp_stream_callback at 0x8004_fa98;
}
