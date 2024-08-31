use std::marker::PhantomData;

use windows::{
    core::{implement, IUnknown, Interface, BSTR},
    Win32::System::{
        Com::{
            CoCreateInstance, CoInitializeEx, CoInitializeSecurity, CoSetProxyBlanket,
            CLSCTX_INPROC_SERVER, CLSCTX_LOCAL_SERVER, COINIT_MULTITHREADED, EOAC_NONE,
            RPC_C_AUTHN_LEVEL_CALL, RPC_C_AUTHN_LEVEL_DEFAULT, RPC_C_IMP_LEVEL_IMPERSONATE,
        },
        Rpc::{RPC_C_AUTHN_WINNT, RPC_C_AUTHZ_NONE},
        Wmi::{
            IUnsecuredApartment, IWbemClassObject, IWbemLocator, IWbemObjectSink,
            IWbemObjectSink_Impl, IWbemServices, UnsecuredApartment, WbemLocator,
            WBEM_FLAG_SEND_STATUS,
        },
    },
};

use super::Error;

#[implement(IWbemObjectSink)]
struct Notifier<'a, F>
where
    F: Fn() + Send + Sync + 'a,
{
    callback: F,
    _marker: PhantomData<&'a ()>,
}

impl<'a, F: Fn() + Send + Sync> Notifier<'a, F> {
    pub fn new(callback: F) -> Self {
        Self {
            callback,
            _marker: PhantomData,
        }
    }
}

impl<F: Fn() + Send + Sync> IWbemObjectSink_Impl for Notifier_Impl<'_, F> {
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
        _lflags: i32,
        _hresult: windows_core::HRESULT,
        _strparam: &windows_core::BSTR,
        _pobjparam: Option<&IWbemClassObject>,
    ) -> windows_core::Result<()> {
        Ok(())
    }
}

pub(crate) fn init_com() -> Result<(), Error> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED)
            .ok()
            .map_err(|e| Error::win32("CoInitializeEx", e))?;

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
        )
        .map_err(|e| Error::win32("CoInitializeSecurity", e))?;
    }
    Ok(())
}

pub struct Observer<'cb> {
    sink: IWbemObjectSink,
    _apartment: IUnsecuredApartment,
    iwbem_services: IWbemServices,
    registered: bool,
    _marker: PhantomData<&'cb ()>,
}

impl<'cb> Observer<'cb> {
    pub fn new<F: Fn() + Send + Sync + 'cb>(callback: F) -> Result<Self, Error> {
        unsafe {
            let iwbem_locator: IWbemLocator =
                CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER)
                    .map_err(|e| Error::win32("CoCreateInstance", e))?;

            let iwbem_services: IWbemServices = iwbem_locator
                .ConnectServer::<&BSTR, _, _, _, _, _>(
                    &"ROOT\\CIMV2".into(),
                    None,
                    None,
                    None,
                    0,
                    None,
                    None,
                )
                .map_err(|e| Error::win32("ConnectServer", e))?;

            CoSetProxyBlanket(
                &iwbem_services,
                RPC_C_AUTHN_WINNT,
                RPC_C_AUTHZ_NONE,
                None,
                RPC_C_AUTHN_LEVEL_CALL,
                RPC_C_IMP_LEVEL_IMPERSONATE,
                None,
                EOAC_NONE,
            )
            .map_err(|e| Error::win32("CoSetProxyBlanket", e))?;

            let apartment: IUnsecuredApartment =
                CoCreateInstance(&UnsecuredApartment, None, CLSCTX_LOCAL_SERVER)
                    .map_err(|e| Error::win32("CoCreateInstance UnsecuredApartment", e))?;

            let notifier: IWbemObjectSink = Notifier::new(callback).into();

            let notifier: IWbemObjectSink = apartment
                .CreateObjectStub(
                    &notifier
                        .cast::<IUnknown>()
                        .map_err(|e| Error::win32("CreateObjectStub", e))?,
                )
                .map_err(|e| Error::win32("CreateObjectStub", e))?
                .cast::<IWbemObjectSink>()
                .map_err(|e| Error::win32("CreateObjectStub.cast", e))?;

            Ok(Self {
                sink: notifier,
                _apartment: apartment,
                iwbem_services,
                registered: false,
                _marker: PhantomData,
            })
        }
    }

    pub fn register(&mut self) -> Result<(), Error> {
        if !self.registered {
            unsafe {
                self.iwbem_services.ExecNotificationQueryAsync(
                    &"WQL".into(),
                    &"SELECT * FROM __InstanceCreationEvent WITHIN 1 WHERE TargetInstance ISA 'Win32_LogicalDisk'".into(),
                    WBEM_FLAG_SEND_STATUS,
                    None,
                    &self.sink,
                ).map_err(|e| Error::win32("ExecNotificationQueryAsync", e))?;
            }
            self.registered = true;
        }
        Ok(())
    }

    pub fn unregister(&mut self) -> Result<(), Error> {
        if self.registered {
            unsafe {
                self.iwbem_services
                    .CancelAsyncCall(&self.sink)
                    .map_err(|e| Error::win32("CancelAsyncCall", e))?;
            }
            self.registered = false;
        }
        Ok(())
    }
}

impl Drop for Observer<'_> {
    fn drop(&mut self) {
        #[allow(clippy::expect_used)]
        self.unregister()
            .expect("Failed to unregister observer but error was not caught");
    }
}
