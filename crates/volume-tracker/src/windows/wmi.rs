use windows::{
    core::*,
    Win32::System::{Com::*, Rpc::*, Wmi::*},
};

#[implement(IWbemObjectSink)]
struct Notifier {
    callback: Box<dyn Fn() + Send + Sync>,
}

impl IWbemObjectSink_Impl for Notifier_Impl {
    fn Indicate(
        &self,
        lobjectcount: i32,
        _apobjarray: *const Option<IWbemClassObject>,
    ) -> windows_core::Result<()> {
        if lobjectcount > 0 {
            log::debug!("IWbemObjectSink::Indicate");
            (self.this.callback)();
        }

        Ok(())
    }
    fn SetStatus(
        &self,
        lflags: i32,
        _hresult: windows_core::HRESULT,
        _strparam: &windows_core::BSTR,
        _pobjparam: Option<&IWbemClassObject>,
    ) -> windows_core::Result<()> {
        match WBEM_STATUS_TYPE(lflags) {
            WBEM_STATUS_COMPLETE => log::debug!("IWbemObjectSink::SetStatus: WBEM_STATUS_COMPLETE"),
            WBEM_STATUS_PROGRESS => log::debug!("IWbemObjectSink::SetStatus: WBEM_STATUS_PROGRESS"),
            WBEM_STATUS_REQUIREMENTS => {
                log::debug!("IWbemObjectSink::SetStatus: WBEM_STATUS_REQUIREMENTS")
            }
            _ => log::debug!("IWbemObjectSink::SetStatus: Unknown({})", lflags),
        }

        Ok(())
    }
}

pub(crate) fn init_com() -> Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;

        CoInitializeSecurity(
            None,
            -1,
            None,
            None,
            RPC_C_AUTHN_LEVEL_DEFAULT,
            RPC_C_IMP_LEVEL_IMPERSONATE,
            None,
            EOAC_NONE,
            None,
        )?;
    }
    Ok(())
}

pub struct WmiObserver {
    iwbem_services: IWbemServices,
    _apartment: IUnsecuredApartment,
    sink: IWbemObjectSink,
}

impl WmiObserver {
    pub fn new(callback: Box<dyn Fn() + Send + Sync>) -> Result<Self> {
        unsafe {
            let iwbem_locator: IWbemLocator =
                CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER)?;

            let iwbem_services: IWbemServices = iwbem_locator
                .ConnectServer::<&BSTR, _, _, _, _, _>(
                    &"ROOT\\CIMV2".into(),
                    None,
                    None,
                    None,
                    0,
                    None,
                    None,
                )?;

            CoSetProxyBlanket(
                &iwbem_services,
                RPC_C_AUTHN_WINNT,
                RPC_C_AUTHZ_NONE,
                None,
                RPC_C_AUTHN_LEVEL_CALL,
                RPC_C_IMP_LEVEL_IMPERSONATE,
                None,
                EOAC_NONE,
            )?;

            let apartment: IUnsecuredApartment =
                CoCreateInstance(&UnsecuredApartment, None, CLSCTX_LOCAL_SERVER)?;

            let notifier: IWbemObjectSink = Notifier { callback }.into();

            let notifier: IWbemObjectSink = apartment
                .CreateObjectStub(&notifier.cast::<IUnknown>()?)?
                .cast::<IWbemObjectSink>()?;

            iwbem_services.ExecNotificationQueryAsync(
                &"WQL".into(),
                &"SELECT * FROM __InstanceCreationEvent WITHIN 1 WHERE TargetInstance ISA 'Win32_LogicalDisk'".into(),
                WBEM_FLAG_SEND_STATUS,
                None,
                &notifier,
            )?;

            Ok(Self {
                iwbem_services,
                _apartment: apartment,
                sink: notifier,
            })
        }
    }
}

impl Drop for WmiObserver {
    fn drop(&mut self) {
        unsafe {
            self.iwbem_services.CancelAsyncCall(&self.sink).ok();
        }
    }
}
